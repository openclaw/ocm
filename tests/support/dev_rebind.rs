use super::*;

fn rebind(
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    target: &Path,
) -> std::process::Output {
    run_ocm(
        cwd,
        env,
        &[
            "dev",
            "rebind",
            "demo",
            "--repo",
            &path_string(target),
            "--json",
        ],
    )
}

#[test]
fn dev_rebind_preserves_state_and_routes_execution_after_old_source_disappears() {
    #[cfg(unix)]
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-retained-state");
    let repo = init_openclaw_repo(&root);
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let created = run_ocm(
        root.path(),
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let before = get_environment("demo", &env, root.path()).unwrap();
    let paths = ocm::store::derive_env_paths(Path::new(&before.root));
    let config = fs::read(&paths.config_path).unwrap();
    let state = paths.state_dir.join("retained-conversation.json");
    fs::write(
        &state,
        r#"{"id":"synthetic-conversation","message":"retain this fixture"}"#,
    )
    .unwrap();
    let workspace = paths.workspace_dir.join("keep.txt");
    fs::write(&workspace, "workspace fixture").unwrap();
    let credentials = paths.state_dir.join("synthetic-credentials.json");
    fs::write(&credentials, r#"{"credential":"synthetic-test-only"}"#).unwrap();
    let user_files = [&paths.config_path, &state, &workspace, &credentials];
    let bytes = user_files.map(|path| fs::read(path).unwrap());
    let target = root.child("next-source");
    init_nested_openclaw_repo(&target);
    fs::create_dir_all(target.join("extensions")).unwrap();
    fs::write(target.join("openclaw.mjs"), "// next source\n").unwrap();
    let target = fs::canonicalize(target).unwrap();
    let old = root.child("retained-old-source");
    fs::rename(&repo, &old).unwrap();
    let old_status = git_worktree_paths(&old);
    fs::remove_file(root.child("node.log")).unwrap();
    let output = rebind(root.path(), &env, &target);
    assert!(output.status.success(), "{}", stderr(&output));
    let summary: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(summary["changed"], true);
    assert_eq!(
        summary["previousSourceRoot"],
        before.dev.as_ref().unwrap().source_root()
    );
    assert_eq!(summary["sourceRoot"], path_string(&target));
    assert!(!root.child("node.log").exists(), "rebind executed source");
    assert!(
        !root.child("pnpm.log").exists(),
        "rebind installed dependencies"
    );
    let mut expected = before.clone();
    let after = get_environment("demo", &env, root.path()).unwrap();
    expected.dev = after.dev.clone();
    expected.updated_at = after.updated_at;
    assert_eq!(after, expected);
    assert_eq!(user_files.map(|path| fs::read(path).unwrap()), bytes);
    let registry = ocm::store::env_registry_path(&env, root.path()).unwrap();
    let registry_bytes = fs::read(&registry).unwrap();
    let repeated = rebind(root.path(), &env, &target.join("."));
    assert!(repeated.status.success(), "{}", stderr(&repeated));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&repeated)).unwrap()["changed"],
        false
    );
    assert_eq!(fs::read(&registry).unwrap(), registry_bytes);
    let status = run_ocm(root.path(), &env, &["dev", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&status)).unwrap()["worktreeRoot"],
        path_string(&target)
    );
    for args in [
        vec!["env", "run", "demo", "--", "--version"],
        vec!["@demo", "--", "--version"],
        dev_plain(&["demo"]),
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let commands = format!(
        "{}{}",
        fs::read_to_string(root.child("node.log")).unwrap(),
        fs::read_to_string(root.child("pnpm.log")).unwrap()
    );
    assert!(
        commands
            .lines()
            .all(|line| line.starts_with(&format!("{}|", target.display()))),
        "{commands}"
    );
    assert!(commands.contains(&format!("bundled={}/extensions", target.display())));
    assert_eq!(fs::read(&paths.config_path).unwrap(), config);
    assert_eq!(user_files.map(|path| fs::read(path).unwrap()), bytes);
    let stopped = run_ocm(root.path(), &env, &["dev", "stop", "demo"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    let removed = run_ocm(root.path(), &env, &["env", "remove", "demo"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(!repo.exists());
    assert_eq!(git_worktree_paths(&old), old_status);
    assert_eq!(
        fs::read_to_string(target.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
    assert_eq!(
        fs::read_to_string(target.join("openclaw.mjs")).unwrap(),
        "// next source\n"
    );
}

#[test]
fn dev_rebind_refusals_preserve_the_binding_and_policy() {
    #[cfg(unix)]
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-refusals");
    let repo = init_openclaw_repo(&root);
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let created = run_ocm(
        root.path(),
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let original = get_environment("demo", &env, root.path()).unwrap();
    let registry = ocm::store::env_registry_path(&env, root.path()).unwrap();
    let unchanged = fs::read(&registry).unwrap();
    for args in [
        vec!["dev", "rebind", "demo"],
        vec![
            "dev",
            "rebind",
            "demo",
            "--repo",
            &path_string(&repo),
            "--force",
        ],
    ] {
        assert!(!run_ocm(root.path(), &env, &args).status.success());
        assert_eq!(fs::read(&registry).unwrap(), unchanged);
    }
    for target in [root.child("absent"), repo.join("scripts")] {
        assert!(!rebind(root.path(), &env, &target).status.success());
        assert_eq!(fs::read(&registry).unwrap(), unchanged);
    }
    let overlap = Path::new(&original.root).join("source");
    init_nested_openclaw_repo(&overlap);
    assert!(!rebind(root.path(), &env, &overlap).status.success());
    assert_eq!(fs::read(&registry).unwrap(), unchanged);
    let operation = hold_environment_operation(&root, "demo");
    let busy = rebind(root.path(), &env, &repo);
    assert!(!busy.status.success());
    assert!(stderr(&busy).contains("operation in progress"));
    assert_eq!(fs::read(&registry).unwrap(), unchanged);
    drop(operation);
    for (runtime, launcher, owned, desired) in [
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        let mut meta = original.clone();
        meta.default_runtime = runtime.then(|| "runtime-fixture".to_string());
        meta.default_launcher = launcher.then(|| "launcher-fixture".to_string());
        if owned {
            meta.dev = Some(EnvDevMeta::Owned {
                repo_root: path_string(&repo),
                worktree_root: path_string(&repo),
            });
        }
        meta.service_running = desired;
        save_environment(meta, &env, root.path()).unwrap();
        let before = fs::read(&registry).unwrap();
        let result = rebind(root.path(), &env, &repo);
        assert!(!result.status.success());
        assert_eq!(fs::read(&registry).unwrap(), before);
    }
    save_environment(original.clone(), &env, root.path()).unwrap();
    let runtime_path = supervisor_runtime_path(&env, root.path()).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime = SupervisorRuntimeState {
        retained_ownership: None,
        kind: "ocm-supervisor-runtime".into(),
        ocm_home: env["OCM_HOME"].clone(),
        daemon_version: None,
        gateway_admission: None,
        updated_at: now_utc(),
        services: vec![],
        children: vec![SupervisorRuntimeChild {
            launch_spec_sha256: None,
            env_name: "demo".into(),
            binding_kind: "dev".into(),
            binding_name: "dev".into(),
            pid: std::process::id(),
            restart_count: 0,
            child_port: original.gateway_port.unwrap(),
            stdout_path: "fixture".into(),
            stderr_path: "fixture".into(),
        }],
    };
    for contents in [
        serde_json::to_vec(&runtime).unwrap(),
        b"unreadable record".to_vec(),
    ] {
        fs::write(&runtime_path, contents).unwrap();
        let before = fs::read(&registry).unwrap();
        assert!(!rebind(root.path(), &env, &repo).status.success());
        assert_eq!(fs::read(&registry).unwrap(), before);
    }
}

#[test]
fn dev_rebind_preserves_an_environment_named_rebind_and_exposes_help() {
    #[cfg(unix)]
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-command-name");
    let repo = init_openclaw_repo(&root);
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    for args in [
        dev_plain(&["rebind", "--repo", &path_string(&repo)]),
        dev_plain(&["rebind"]),
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let help = run_ocm(root.path(), &env, &["dev", "rebind", "--help"]);
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("dev rebind <env> --repo <checkout>"));
}

#[cfg(unix)]
#[test]
fn dev_rebind_refuses_active_and_unfinished_foreground_ownership() {
    #[cfg(unix)]
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-foreground");
    let repo = init_openclaw_repo(&root);
    let mut env = service_env(&root);
    let (started, _, _) = install_blocking_fake_dev_runners(&root, &mut env);
    let mut watch = DevWatchFixture::spawn(
        &root,
        root.path(),
        &env,
        &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
    );
    assert!(wait_for_path(&started, Duration::from_secs(10)));
    let registry = ocm::store::env_registry_path(&env, root.path()).unwrap();
    let before = fs::read(&registry).unwrap();
    let refused = rebind(root.path(), &env, &repo);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("dev session is"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(fs::read(&registry).unwrap(), before);
    let stop = run_named_dev_stop(root.path(), &env, "demo");
    assert!(stop.status.success(), "{}", stderr(&stop));
    watch.wait_without_release();
    let session_path = source_watch_override_path(&root, "demo").with_extension("session");
    let original = fs::read(&session_path).unwrap();
    let mut session: Value = serde_json::from_slice(&original).unwrap();
    session["closed"] = false.into();
    fs::write(&session_path, serde_json::to_vec(&session).unwrap()).unwrap();
    let before = fs::read(&registry).unwrap();
    let unfinished = rebind(root.path(), &env, &repo);
    assert!(!unfinished.status.success());
    assert!(
        stderr(&unfinished).contains("unfinished"),
        "{}",
        stderr(&unfinished)
    );
    assert_eq!(fs::read(&registry).unwrap(), before);
    fs::write(&session_path, original).unwrap();
    let repeated = rebind(root.path(), &env, &repo);
    assert!(repeated.status.success(), "{}", stderr(&repeated));
}

#[cfg(unix)]
struct RegistryWriteFailure {
    path: PathBuf,
    permissions: fs::Permissions,
}

#[cfg(unix)]
impl RegistryWriteFailure {
    fn new(registry: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let path = registry.parent().unwrap().to_path_buf();
        let permissions = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500)).unwrap();
        Self { path, permissions }
    }
}

#[cfg(unix)]
impl Drop for RegistryWriteFailure {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, self.permissions.clone()).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn dev_rebind_failed_registry_publication_preserves_previous_binding() {
    #[cfg(unix)]
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-write-failure");
    let repo = init_openclaw_repo(&root);
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let created = run_ocm(
        root.path(),
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let target = root.child("next");
    init_nested_openclaw_repo(&target);
    let registry = ocm::store::env_registry_path(&env, root.path()).unwrap();
    let bytes = fs::read(&registry).unwrap();
    let fault = RegistryWriteFailure::new(&registry);
    let result = rebind(root.path(), &env, &target);
    drop(fault);
    assert!(
        !result.status.success(),
        "publication unexpectedly succeeded"
    );
    assert_eq!(fs::read(&registry).unwrap(), bytes);
}

#[test]
fn dev_rebind_rejects_old_service_plan_and_next_start_resolves_new_source() {
    #[cfg(unix)]
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-service-plan");
    let repo = init_openclaw_repo(&root);
    let mut env = service_env_with_gateway_admission(&root);
    install_fake_dev_runners(&root, &mut env);
    let created = run_ocm(
        root.path(),
        &env,
        &["dev", "demo", "--repo", &path_string(&repo), "--service"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let state_path = ocm::store::supervisor_state_path(&env, root.path()).unwrap();
    let old_plan = fs::read(&state_path).unwrap();
    let stopped = run_ocm(root.path(), &env, &["service", "stop", "demo"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    let target = root.child("next-source");
    init_nested_openclaw_repo(&target);
    let target = fs::canonicalize(target).unwrap();
    let rebound = rebind(root.path(), &env, &target);
    assert!(rebound.status.success(), "{}", stderr(&rebound));
    let meta = get_environment("demo", &env, root.path()).unwrap();
    assert!(meta.service_enabled);
    assert!(!meta.service_running);
    // A daemon can retain an older plan while metadata changes. Admission must
    // reject that exact plan even if a later writer requests a running service.
    let mut running = meta;
    running.service_running = true;
    save_environment(running, &env, root.path()).unwrap();
    fs::write(&state_path, old_plan).unwrap();
    let _ = fs::remove_file(root.child("node.log"));
    let stale = run_ocm(root.path(), &env, &["__daemon", "run", "--once"]);
    assert!(!stale.status.success());
    assert!(
        stderr(&stale).contains("saved dev source no longer matches"),
        "{}",
        stderr(&stale)
    );
    assert!(!root.child("node.log").exists());
    let started = run_ocm(root.path(), &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let plan: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(plan["children"][0]["runDir"], path_string(&target));
    let launched = run_ocm(root.path(), &env, &["__daemon", "run", "--once"]);
    assert!(launched.status.success(), "{}", stderr(&launched));
    assert!(
        fs::read_to_string(root.child("pnpm.log"))
            .unwrap()
            .starts_with(&format!("{}|", target.display()))
    );
    let stopped = run_ocm(root.path(), &env, &["service", "stop", "demo"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    let removed = run_ocm(root.path(), &env, &["env", "remove", "demo"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
}

#[cfg(unix)]
#[test]
fn dev_rebind_retains_ui_port_and_serves_new_code_with_saved_conversation() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-live-ui");
    let mut env = ocm_env(&root);
    let repo = prepare_initial_ui_repo(&root, &mut env);
    let mut controller = DevWatchFixture::spawn(
        &root,
        root.path(),
        &env,
        // Other fixture stores probe from 18789 upward even while this
        // environment is stopped. Keep its retained family below that range.
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--port",
            "16789",
        ],
    );
    let gateway = initial_ui_process(&root, "gateway", &mut controller);
    let ui = initial_ui_process(&root, "ui", &mut controller);
    let saved = get_environment("demo", &env, root.path()).unwrap();
    let paths = ocm::store::derive_env_paths(Path::new(&saved.root));
    let conversation = paths.state_dir.join("retained-conversation.txt");
    fs::write(&conversation, "saved synthetic conversation").unwrap();
    let config = fs::read(&paths.config_path).unwrap();
    let stopped = run_named_dev_stop(root.path(), &env, "demo");
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(controller.wait_without_release().status.code(), Some(130));
    let target = root.child("next-source");
    let cloned = Command::new("git")
        .args(["clone", &path_string(&repo), &path_string(&target)])
        .output()
        .unwrap();
    assert!(cloned.status.success(), "{}", stderr(&cloned));
    let entry = target.join("scripts/dev-ui-fixture.cjs");
    let code = fs::read_to_string(&entry).unwrap().replace(
        "\"<!doctype html><title>OCM UI fixture</title>\"",
        "'<!doctype html><title>NEW SOURCE</title>' + fs.readFileSync(path.join(process.env.OPENCLAW_STATE_DIR, 'retained-conversation.txt'), 'utf8')",
    );
    fs::write(&entry, &code).unwrap();
    fs::remove_dir_all(&repo).unwrap();
    let target = fs::canonicalize(target).unwrap();
    let registry = ocm::store::env_registry_path(&env, root.path()).unwrap();
    let previous_binding = fs::read(&registry).unwrap();
    let occupied =
        std::net::TcpListener::bind(("127.0.0.1", saved.gateway_port.unwrap() as u16 + 1)).unwrap();
    let refused = rebind(root.path(), &env, &target);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("Gateway port family is in use"));
    assert_eq!(fs::read(&registry).unwrap(), previous_binding);
    assert_eq!(fs::read(&paths.config_path).unwrap(), config);
    assert_eq!(
        fs::read_to_string(&conversation).unwrap(),
        "saved synthetic conversation"
    );
    drop(occupied);
    let rebound = rebind(root.path(), &env, &target);
    assert!(rebound.status.success(), "{}", stderr(&rebound));
    assert_eq!(
        get_environment("demo", &env, root.path())
            .unwrap()
            .dev_ui_port,
        saved.dev_ui_port
    );
    assert_eq!(fs::read(&paths.config_path).unwrap(), config);
    assert_eq!(fs::read_to_string(&entry).unwrap(), code);
    for role in ["gateway", "ui"] {
        fs::remove_file(root.child(format!("{role}.json"))).unwrap();
    }
    let mut next = DevWatchFixture::spawn(
        &root,
        root.path(),
        &env,
        &["dev", "demo", "--repo", &path_string(&target)],
    );
    let next_gateway = initial_ui_process(&root, "gateway", &mut next);
    let next_ui = initial_ui_process(&root, "ui", &mut next);
    assert_eq!(next_gateway["port"], gateway["port"]);
    assert_eq!(next_ui["port"], ui["port"]);
    assert_eq!(next_gateway["cwd"], path_string(&target));
    assert_eq!(next_ui["cwd"], path_string(&target));
    let mut response = ureq::get(format!("http://127.0.0.1:{}/", next_ui["port"]))
        .call()
        .unwrap();
    let document = response.body_mut().read_to_string().unwrap();
    assert!(document.contains("NEW SOURCE"), "{document}");
    assert!(
        document.contains("saved synthetic conversation"),
        "{document}"
    );
    assert_eq!(fs::read(&paths.config_path).unwrap(), config);
    let stopped = run_named_dev_stop(root.path(), &env, "demo");
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(next.wait_without_release().status.code(), Some(130));
    let removed = run_ocm(root.path(), &env, &["env", "remove", "demo"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert_eq!(fs::read_to_string(&entry).unwrap(), code);
    assert!(!repo.exists());
}

#[cfg(unix)]
#[test]
fn dev_rebind_waits_for_stopped_history_retirement() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-rebind-stopped-history");
    let repo = init_openclaw_repo(&root);
    let mut env = service_env_with_gateway_admission(&root);
    install_fake_dev_runners(&root, &mut env);
    for name in ["demo", "sibling"] {
        let created = run_ocm(
            root.path(),
            &env,
            &["dev", name, "--repo", &path_string(&repo), "--service"],
        );
        assert!(created.status.success(), "{}", stderr(&created));
    }
    // Setup advertised this test process as a synthetic empty daemon. Retire
    // that fixture record before launching the actual publisher; a real live
    // publisher must never be replaced just because its current rows are empty.
    let runtime_path = supervisor_runtime_path(&env, root.path()).unwrap();
    let setup_runtime: SupervisorRuntimeState =
        serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
    assert!(setup_runtime.children.is_empty() && setup_runtime.services.is_empty());
    fs::remove_file(&runtime_path).unwrap();
    let mut daemon = DevWatchFixture::spawn_caller(&root, root.path(), &env, &["__daemon", "run"]);
    let pid = daemon.child.as_ref().unwrap().id();
    env.insert("OCM_TEST_NATIVE_DAEMON_PID".into(), pid.to_string());
    if cfg!(target_os = "macos") {
        let definition = support::managed_service_definition_path(&env, root.path(), "demo");
        fs::write(
            root.child("launchctl-print.txt"),
            format!(
                "state = running\npid = {pid}\npath = {}\n",
                definition.display()
            ),
        )
        .unwrap();
    }
    let runtime_path = supervisor_runtime_path(&env, root.path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if !runtime_path.exists() {
            assert!(
                Instant::now() < deadline,
                "daemon never published its runtime"
            );
            thread::sleep(Duration::from_millis(25));
            continue;
        }
        let runtime: SupervisorRuntimeState =
            serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
        if runtime.services.iter().any(|service| {
            service.env_name == "demo"
                && service.gateway_state == "stopped"
                && service.pid.is_none()
        }) {
            assert!(
                !runtime
                    .children
                    .iter()
                    .any(|child| child.env_name == "demo")
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not publish stopped history: {runtime:?}; controller={:?}",
            daemon.child.as_mut().unwrap().try_wait()
        );
        thread::sleep(Duration::from_millis(25));
    }
    let stopped_history = fs::read(&runtime_path).unwrap();
    let stopped = run_ocm(root.path(), &env, &["service", "stop", "demo"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    let target = root.child("next-source");
    init_nested_openclaw_repo(&target);
    let rebound = rebind(root.path(), &env, &target);
    assert!(rebound.status.success(), "{}", stderr(&rebound));
    let runtime: SupervisorRuntimeState =
        serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
    assert!(
        !runtime
            .services
            .iter()
            .any(|service| service.env_name == "demo")
    );
    let current = get_environment("demo", &env, root.path()).unwrap();
    assert!(current.service_enabled);
    assert!(!current.service_running);
    // The same history after its daemon exits is no longer authoritative.
    daemon.crash_controller();
    fs::write(&runtime_path, stopped_history).unwrap();
    let bytes = fs::read(ocm::store::env_registry_path(&env, root.path()).unwrap()).unwrap();
    let stale = rebind(root.path(), &env, &repo);
    assert!(!stale.status.success());
    assert_eq!(
        fs::read(ocm::store::env_registry_path(&env, root.path()).unwrap()).unwrap(),
        bytes
    );
}

#[cfg(unix)]
struct ReleasePreparation(PathBuf);

#[cfg(unix)]
impl Drop for ReleasePreparation {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, "release\n");
    }
}

#[cfg(unix)]
#[test]
fn preparation_rollback_preserves_source_owners_and_cleans_only_unused_worktrees() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for mode in [
        "active",
        "none",
        "disjoint",
        "completed",
        "permanent",
        "unknown",
        "dirty",
    ] {
        let root = TestDir::new(&format!("preparation-rollback-{mode}"));
        let repo = fs::canonicalize(init_openclaw_repo(&root)).unwrap();
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env(&root);
        let (started, release, _) = install_blocking_fake_dev_runners(&root, &mut env);
        create_runtime_backed_env(&cwd, &env);
        let created = run_ocm(
            &cwd,
            &env,
            &["env", "create", "source", "--runtime", "stable"],
        );
        assert!(created.status.success(), "{}", stderr(&created));

        let real_git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(real_git.status.success());
        let gate = root.child("worktree-added");
        let gate_release = root.child("release-preparation");
        let tools = root.child("git-gate");
        fs::create_dir_all(&tools).unwrap();
        write_executable_script(
            &tools.join("git"),
            r#"#!/bin/sh
"$OCM_TEST_REAL_GIT" "$@"
status=$?
if [ "$status" = 0 ] && [ "$3" = worktree ] && [ "$4" = add ]; then
  printf '%s/%s\n' "$2" "$6" > "$OCM_TEST_GIT_GATE.tmp"
  mv "$OCM_TEST_GIT_GATE.tmp" "$OCM_TEST_GIT_GATE"
  attempts=0
  while [ ! -f "$OCM_TEST_GIT_RELEASE" ]; do
    attempts=$((attempts + 1))
    [ "$attempts" -lt 600 ] || exit 92
    /bin/sleep 0.05
  done
fi
exit "$status"
"#,
        );
        let mut creator_env = env.clone();
        prepend_fake_bin(&mut creator_env, &tools);
        creator_env.insert("OCM_TEST_REAL_GIT".into(), stdout(&real_git).trim().into());
        creator_env.insert("OCM_TEST_GIT_GATE".into(), path_string(&gate));
        creator_env.insert("OCM_TEST_GIT_RELEASE".into(), path_string(&gate_release));
        let mut creator = DevWatchFixture::spawn_caller(
            &root,
            &cwd,
            &creator_env,
            &[
                "upgrade",
                "simulate",
                "source",
                "--to",
                &path_string(&repo),
                "--scenario",
                "current",
                "--keep-simulation",
                "--json",
            ],
        );
        // Release before the caller fixture reaps the command on assertion failure.
        let _gate_cleanup = ReleasePreparation(gate_release.clone());
        assert!(
            wait_for_path(&gate, Duration::from_secs(30)),
            "{mode}: worktree not prepared"
        );
        let worktree = fs::canonicalize(fs::read_to_string(&gate).unwrap().trim()).unwrap();
        assert_eq!(worktree.parent().unwrap(), repo.join(".worktrees"));
        let simulation = worktree.file_name().unwrap().to_str().unwrap();
        assert!(
            get_environment(simulation, &env, &cwd)
                .unwrap()
                .dev
                .is_none()
        );
        let sentinel = worktree.join("openclaw.mjs");
        let source_bytes = fs::read(&sentinel).unwrap();
        let mut watch = None;
        let owner = if mode == "permanent" {
            "borrower"
        } else {
            "demo"
        };
        let selected = if mode == "disjoint" { &repo } else { &worktree };
        if matches!(
            mode,
            "active" | "disjoint" | "completed" | "permanent" | "unknown"
        ) {
            let mut args = vec![
                "dev",
                owner,
                "--repo",
                selected.to_str().unwrap(),
                "--watch",
                "--no-ui",
            ];
            if mode != "permanent" {
                args.push("--force");
            }
            watch = Some(DevWatchFixture::spawn(&root, &cwd, &env, &args));
            assert!(
                wait_for_path(&started, Duration::from_secs(30)),
                "{mode}: watch not started"
            );
        }
        let override_path = source_watch_override_path(&root, owner);
        let ownership = watch.as_ref().map(|_| fs::read(&override_path).unwrap());
        if matches!(mode, "completed" | "permanent") {
            fs::write(&release, "release\n").unwrap();
            let completed = watch.as_mut().unwrap().wait_without_release();
            assert!(completed.status.success(), "{}", stderr(&completed));
            let session: Value =
                serde_json::from_slice(&fs::read(override_path.with_extension("session")).unwrap())
                    .unwrap();
            assert_eq!(session["closed"], true);
            watch = None;
        }
        if mode == "dirty" {
            fs::write(&sentinel, "uncommitted source\n").unwrap();
        }
        let registry = ocm::store::env_registry_path(&env, &cwd).unwrap();
        let registry_bytes = fs::read(&registry).unwrap();
        if mode == "unknown" {
            // A valid unfinished owner without its source publication must
            // retain the checkout too; release only after restoring the record.
            fs::remove_file(&override_path).unwrap();
        }
        let fault = RegistryWriteFailure::new(&registry);
        fs::write(&gate_release, "release\n").unwrap();
        let output = creator.wait_without_release();
        drop(fault);
        if mode == "unknown" {
            fs::write(&override_path, ownership.as_ref().unwrap()).unwrap();
        }
        let detail = format!("{}{}", stdout(&output), stderr(&output));
        assert!(
            !output.status.success(),
            "{mode}: publication unexpectedly succeeded"
        );
        let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
        let preparation = summary["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stage| stage["name"] == "prepare target")
            .unwrap();
        assert_eq!(preparation["status"], "failed", "{mode}: {detail}");
        let failure = preparation["note"].as_str().unwrap();
        assert!(
            failure.starts_with("Permission denied")
                || failure.starts_with("Operation not permitted"),
            "{mode}: {detail}"
        );
        assert_eq!(fs::read(&registry).unwrap(), registry_bytes);
        let retained = matches!(mode, "active" | "permanent" | "unknown" | "dirty");
        assert_eq!(worktree.exists(), retained, "{mode}: {detail}");
        assert_eq!(
            git_worktree_paths(&repo).contains(&worktree),
            retained,
            "{mode}: Git registration"
        );
        if retained {
            assert!(
                detail.contains(&format!(
                    "prepared worktree retained at {}",
                    worktree.display()
                )),
                "{detail}"
            );
            if mode == "dirty" {
                assert!(detail.contains("modified or untracked files"), "{detail}");
                assert_eq!(
                    fs::read_to_string(&sentinel).unwrap(),
                    "uncommitted source\n"
                );
            } else {
                assert_eq!(fs::read(&sentinel).unwrap(), source_bytes);
                let reason = match mode {
                    "active" => "active temporary dev source for env demo",
                    "permanent" => "registered dev source for env borrower",
                    _ => "cannot establish temporary dev source ownership for env demo",
                };
                assert!(detail.contains(reason), "{mode}: {detail}");
            }
        } else {
            assert!(
                !detail.contains("prepared worktree retained"),
                "{mode}: {detail}"
            );
        }
        if let Some(mut watch) = watch {
            assert!(watch.child.as_mut().unwrap().try_wait().unwrap().is_none());
            assert_eq!(fs::read(&override_path).unwrap(), ownership.unwrap());
            assert!(selected.join("openclaw.mjs").exists());
            fs::write(&release, "release\n").unwrap();
            let completed = watch.wait_without_release();
            assert!(completed.status.success(), "{}", stderr(&completed));
        }
    }
}
