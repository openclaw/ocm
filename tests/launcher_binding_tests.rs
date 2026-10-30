mod support;

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};
use fs2::FileExt;
use ocm::store::env_registry_path;

#[test]
fn env_set_launcher_updates_and_clears_the_default_launcher() {
    let root = TestDir::new("launcher-binding-set");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "sh"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let bind = run_ocm(&cwd, &env, &["env", "set-launcher", "demo", "stable"]);
    assert!(bind.status.success(), "{}", stderr(&bind));
    assert!(stdout(&bind).contains("Updated env demo: defaultLauncher=stable"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("defaultLauncher: stable"));

    let clear = run_ocm(&cwd, &env, &["env", "set-launcher", "demo", "none"]);
    assert!(clear.status.success(), "{}", stderr(&clear));
    assert!(stdout(&clear).contains("Updated env demo: defaultLauncher cleared"));
    assert!(!stdout(&clear).contains("defaultLauncher=none"));

    let show_cleared = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(show_cleared.status.success(), "{}", stderr(&show_cleared));
    assert!(!stdout(&show_cleared).contains("defaultLauncher:"));
}

#[test]
fn launcher_bindings_require_an_existing_launcher() {
    let root = TestDir::new("launcher-binding-missing");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "invalid",
            "--root",
            "./orphan",
            "--launcher",
            "missing",
        ],
    );
    assert!(!create.status.success());
    assert!(stderr(&create).contains("launcher \"missing\" does not exist"));
    assert!(!cwd.join("orphan").exists());

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let bind = run_ocm(&cwd, &env, &["env", "set-launcher", "demo", "missing"]);
    assert!(!bind.status.success());
    assert!(stderr(&bind).contains("launcher \"missing\" does not exist"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let environment: serde_json::Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert!(environment["defaultLauncher"].is_null());

    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    registry["envs"][0]["defaultLauncher"] = "missing".into();
    fs::write(
        registry_path,
        format!("{}\n", serde_json::to_string_pretty(&registry).unwrap()),
    )
    .unwrap();
    let bind = run_ocm(&cwd, &env, &["env", "set-launcher", "demo", "missing"]);
    assert!(!bind.status.success());
    assert!(stderr(&bind).contains("launcher \"missing\" does not exist"));
}

#[test]
fn case_variant_launcher_bindings_use_metadata_identity_when_supported() {
    let root = TestDir::new("launcher-binding-case-variant");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "sh"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "STABLE"],
    );
    if !create.status.success() {
        assert!(stderr(&create).contains("launcher \"STABLE\" does not exist"));
        return;
    }

    let environment = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(environment.status.success(), "{}", stderr(&environment));
    let environment: serde_json::Value = serde_json::from_str(&stdout(&environment)).unwrap();
    assert_eq!(environment["defaultLauncher"], "stable");

    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    registry["envs"][0]["defaultLauncher"] = "STABLE".into();
    fs::write(
        registry_path,
        format!("{}\n", serde_json::to_string_pretty(&registry).unwrap()),
    )
    .unwrap();

    let remove = run_ocm(&cwd, &env, &["launcher", "remove", "stable"]);
    assert!(!remove.status.success());
    assert!(stderr(&remove).contains("is still used by environment(s): demo"));
}

#[test]
fn launcher_binding_and_removal_share_the_environment_registry_lock() {
    let root = TestDir::new("launcher-binding-remove-lock");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "sh"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let lock_path = env_registry_path(&env, &cwd)
        .unwrap()
        .with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    lock.lock_exclusive().unwrap();

    let mut bind = Command::new(env!("CARGO_BIN_EXE_ocm"));
    bind.current_dir(&cwd)
        .args(["env", "set-launcher", "demo", "stable"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut bind = bind.spawn().unwrap();
    thread::sleep(Duration::from_millis(100));

    let mut remove = Command::new(env!("CARGO_BIN_EXE_ocm"));
    remove
        .current_dir(&cwd)
        .args(["launcher", "remove", "stable"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut remove = remove.spawn().unwrap();
    thread::sleep(Duration::from_millis(100));

    assert!(bind.try_wait().unwrap().is_none());
    assert!(remove.try_wait().unwrap().is_none());
    FileExt::unlock(&lock).unwrap();

    let bind = bind.wait_with_output().unwrap();
    let remove = remove.wait_with_output().unwrap();
    assert_ne!(bind.status.success(), remove.status.success());

    let environment = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(environment.status.success(), "{}", stderr(&environment));
    let environment: serde_json::Value = serde_json::from_str(&stdout(&environment)).unwrap();
    let launcher = run_ocm(&cwd, &env, &["launcher", "show", "stable"]);
    if bind.status.success() {
        assert!(launcher.status.success(), "{}", stderr(&launcher));
        assert_eq!(environment["defaultLauncher"], "stable");
    } else {
        assert!(!launcher.status.success());
        assert!(environment["defaultLauncher"].is_null());
    }
}

#[test]
fn env_set_launcher_clears_an_existing_runtime_binding() {
    let root = TestDir::new("launcher-binding-clears-runtime");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    fs::write(&runtime_path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&runtime_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&runtime_path, permissions).unwrap();
    }
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));

    let add_launcher = run_ocm(&cwd, &env, &["launcher", "add", "dev", "--command", "sh"]);
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let bind = run_ocm(&cwd, &env, &["env", "set-launcher", "demo", "dev"]);
    assert!(bind.status.success(), "{}", stderr(&bind));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("defaultLauncher: dev"));
    assert!(!output.contains("defaultRuntime:"));
}

#[test]
fn env_run_without_a_launcher_uses_launcher_specific_guidance() {
    let root = TestDir::new("launcher-binding-run-error");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--", "onboard"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains(
        "environment \"demo\" has no default runtime, launcher, or dev binding; use ocm dev <name>, env set-runtime, env set-launcher, or pass --runtime/--launcher",
    ));
}
