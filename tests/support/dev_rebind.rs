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
#[test]
fn dev_rebind_failed_registry_publication_preserves_previous_binding() {
    #[cfg(unix)]
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    use std::os::unix::fs::PermissionsExt;
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
    let parent = registry.parent().unwrap();
    let permissions = fs::metadata(parent).unwrap().permissions();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o500)).unwrap();
    let result = rebind(root.path(), &env, &target);
    fs::set_permissions(parent, permissions).unwrap();
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
        &["dev", "demo", "--repo", &path_string(&repo)],
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
