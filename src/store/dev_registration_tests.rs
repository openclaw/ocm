use std::process::Command;

use super::*;
use crate::env::CreateEnvironmentOptions;
use crate::store::{
    create_environment, create_environment_with_validated_runtime, derive_env_paths,
    get_environment, lock_environment_operation, save_environment,
    save_environment_with_dev_registration, save_environment_with_validated_launcher,
    save_environment_with_validated_runtime,
};

type Save = fn(EnvMeta, &BTreeMap<String, String>, &Path) -> Result<EnvMeta, String>;
const SAVES: [Save; 3] = [
    save_environment,
    save_environment_with_validated_launcher,
    save_environment_with_validated_runtime,
];

fn options(name: &str, dev: Option<EnvDevMeta>) -> CreateEnvironmentOptions {
    CreateEnvironmentOptions {
        name: name.to_string(),
        root: None,
        gateway_port: Some(19789),
        service_enabled: false,
        service_running: false,
        default_runtime: None,
        default_launcher: None,
        dev,
        protected: false,
    }
}

fn owned_source(repo: &Path) -> EnvDevMeta {
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::write(repo.join("package.json"), r#"{"name":"openclaw"}"#).unwrap();
    fs::write(repo.join("scripts/run-node.mjs"), "").unwrap();
    for args in [&["init"][..], &["add", "."], &["commit", "-m", "fixture"]] {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repo.join("unused-global-config"))
            .args([
                "-c",
                "user.name=OCM Tests",
                "-c",
                "user.email=tests@example.invalid",
            ])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let worktree = ensure_openclaw_worktree(repo, "child").unwrap();
    assert!(worktree.created);
    EnvDevMeta::Owned {
        repo_root: display_path(repo),
        worktree_root: display_path(&worktree.root),
    }
}

fn fixture() -> (tempfile::TempDir, BTreeMap<String, String>, EnvDevMeta) {
    let temp = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([
        ("HOME".to_string(), display_path(&temp.path().join("home"))),
        (
            "OCM_HOME".to_string(),
            display_path(&temp.path().join("ocm-home")),
        ),
    ]);
    let parent = create_environment(options("parent", None), &env, temp.path()).unwrap();
    let repo = derive_env_paths(Path::new(&parent.root))
        .workspace_dir
        .join("openclaw");
    let dev = owned_source(&repo);
    (temp, env, dev)
}

#[test]
fn every_publisher_refuses_a_new_or_changed_source_during_its_owners_operation() {
    assert_publishers_preserve_busy_source(false);
    assert_publishers_preserve_busy_source(true);
}

fn assert_publishers_preserve_busy_source(borrowed: bool) {
    let (temp, env, owned) = fixture();
    let dev = if borrowed {
        EnvDevMeta::Borrowed {
            source_root: display_path(&fs::canonicalize(owned.source_root()).unwrap()),
        }
    } else {
        owned.clone()
    };
    let cwd = temp.path();
    let mut changed = create_environment(options("child", Some(dev.clone())), &env, cwd).unwrap();
    let replacement =
        ensure_openclaw_worktree(Path::new(owned.repo_root()), "replacement").unwrap();
    changed.dev = Some(if borrowed {
        EnvDevMeta::Borrowed {
            source_root: display_path(&fs::canonicalize(&replacement.root).unwrap()),
        }
    } else {
        EnvDevMeta::Owned {
            repo_root: owned.repo_root().to_string(),
            worktree_root: display_path(&replacement.root),
        }
    });
    let _owner = lock_environment_operation("parent", &env, cwd).unwrap();
    for save in SAVES {
        let error = save(changed.clone(), &env, cwd).unwrap_err();
        assert!(
            error.contains("parent has an operation in progress"),
            "{error}"
        );
    }
    for create in [
        create_environment,
        create_environment_with_validated_runtime,
    ] {
        let error = create(options("new-child", changed.dev.clone()), &env, cwd).unwrap_err();
        assert!(
            error.contains("parent has an operation in progress"),
            "{error}"
        );
    }
    let current = get_environment("child", &env, cwd).unwrap();
    assert_eq!(current.dev.as_ref(), Some(&dev));
    assert!(get_environment("new-child", &env, cwd).is_err());
}

#[test]
fn unchanged_source_saves_allow_locked_recovery_with_displaced_source() {
    let (temp, env, dev) = fixture();
    let cwd = temp.path();
    let mut child = create_environment(options("child", Some(dev.clone())), &env, cwd).unwrap();
    let mut pending = create_environment(options("pending", None), &env, cwd).unwrap();
    pending.dev = Some(dev.clone());
    let _owner = lock_environment_operation("parent", &env, cwd).unwrap();
    let _target = lock_environment_operation("child", &env, cwd).unwrap();
    let parent = get_environment("parent", &env, cwd).unwrap();
    fs::rename(&parent.root, temp.path().join("displaced-parent")).unwrap();
    for save in SAVES {
        child.protected = !child.protected;
        save(child.clone(), &env, cwd).unwrap();
        let current = get_environment("child", &env, cwd).unwrap();
        assert_eq!(current.protected, child.protected);
        assert_eq!(current.dev.as_ref(), Some(&dev));
    }
    drop(_target);
    for save in SAVES {
        assert!(
            save(pending.clone(), &env, cwd)
                .unwrap_err()
                .contains("parent has an operation in progress")
        );
    }
    let error = create_environment(options("new-child", Some(dev.clone())), &env, cwd).unwrap_err();
    assert!(
        error.contains("parent has an operation in progress"),
        "{error}"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(temp.path().join("missing-parent"), &parent.root).unwrap();
        let independent = owned_source(&temp.path().join("independent"));
        create_environment(options("independent", Some(independent)), &env, cwd).unwrap();
        let error =
            create_environment(options("new-child", Some(dev.clone())), &env, cwd).unwrap_err();
        assert!(
            error.contains("parent has an operation in progress"),
            "{error}"
        );
    }
    drop(_owner);
    create_environment(options("new-child", Some(dev)), &env, cwd).unwrap();
}

#[test]
fn publication_rechecks_late_owners_and_replaced_source_without_taking_locks() {
    let (temp, env, dev) = fixture();
    let cwd = temp.path();
    let mut child = create_environment(options("child", None), &env, cwd).unwrap();
    let mut late = create_environment(options("late", None), &env, cwd).unwrap();
    child.dev = Some(dev.clone());
    let registration =
        DevSourceRegistration::acquire("child", child.dev.as_ref(), &env, cwd).unwrap();
    let original_root = late.root.clone();
    late.root = dev.repo_root().to_string();
    save_environment(late.clone(), &env, cwd).unwrap();
    let late_lock = environment_operation_lock_path("late", &env, cwd).unwrap();
    assert!(!late_lock.exists());
    let error = save_environment_with_dev_registration(child.clone(), &registration, &env, cwd)
        .unwrap_err();
    assert!(error.contains("changed during registration"), "{error}");
    assert!(
        !late_lock.exists(),
        "registry recheck attempted to acquire a new owner lock"
    );
    late.root = original_root;
    save_environment(late, &env, cwd).unwrap();
    fs::rename(dev.repo_root(), temp.path().join("original-source")).unwrap();
    let replacement = owned_source(Path::new(dev.repo_root()));
    assert_eq!(replacement, dev);
    let error =
        save_environment_with_dev_registration(child, &registration, &env, cwd).unwrap_err();
    assert!(error.contains("changed during registration"), "{error}");
    assert!(get_environment("child", &env, cwd).unwrap().dev.is_none());
}
