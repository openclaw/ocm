mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use ocm::env::{CreateEnvironmentOptions, EnvDevMeta, EnvMeta};
use ocm::store::{create_environment, env_registry_path};

use support::{TestDir, ocm_env, path_string, run_ocm, stderr, write_text};

fn create(
    name: &str,
    root: Option<&Path>,
    dev: Option<EnvDevMeta>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> EnvMeta {
    create_environment(
        CreateEnvironmentOptions {
            name: name.to_string(),
            root: root.map(path_string),
            gateway_port: None,
            service_enabled: false,
            service_running: false,
            default_runtime: None,
            default_launcher: None,
            dev,
            protected: false,
        },
        env,
        cwd,
    )
    .unwrap()
}

fn dev_binding(source: &Path) -> EnvDevMeta {
    EnvDevMeta {
        repo_root: path_string(source.parent().unwrap()),
        worktree_root: path_string(source),
    }
}

#[test]
fn create_clone_and_import_reject_registered_dev_source_destinations() {
    let fixture = TestDir::new("registered-dev-source-destinations");
    let cwd = fixture.path();
    let env = ocm_env(&fixture);
    let source = fixture.child("repo/worktree");
    write_text(&source.join("authored.txt"), "preserve source\n");
    create("developer", None, Some(dev_binding(&source)), &env, cwd);
    let template = create("template", None, None, &env, cwd);
    let archive = fixture.child("template.tar");
    let export = run_ocm(
        cwd,
        &env,
        &[
            "env",
            "export",
            "template",
            "--output",
            &path_string(&archive),
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let mut destinations = vec![
        source.join("new/state"),
        source.clone(),
        source.parent().unwrap().to_path_buf(),
    ];
    #[cfg(unix)]
    {
        let alias = fixture.child("source-alias");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        destinations.push(alias.join("new/state"));
        let outside = fixture.child("outside");
        fs::create_dir_all(&outside).unwrap();
        let authored_link = source.join("state-link");
        std::os::unix::fs::symlink(&outside, &authored_link).unwrap();
        destinations.push(authored_link);
    }
    #[cfg(target_os = "macos")]
    {
        let canonical = fs::canonicalize(&source).unwrap();
        let data_alias =
            Path::new("/System/Volumes/Data").join(canonical.strip_prefix("/").unwrap());
        if data_alias.is_dir() {
            destinations.push(data_alias.join("new/state"));
        }
    }
    let registry = env_registry_path(&env, cwd).unwrap();
    let registry_before = fs::read(&registry).unwrap();
    for destination in destinations {
        for action in ["create", "clone", "import"] {
            let name = format!("blocked-{action}");
            let archive = path_string(&archive);
            let destination = path_string(&destination);
            let mut args = match action {
                "create" => vec!["env", "create", &name],
                "clone" => vec!["env", "clone", "template", &name],
                _ => vec!["env", "import", &archive, "--name", &name],
            };
            args.extend(["--root", &destination]);
            let rejected = run_ocm(cwd, &env, &args);
            assert!(!rejected.status.success());
            assert!(
                stderr(&rejected).contains("overlaps dev source"),
                "{}",
                stderr(&rejected)
            );
            assert_eq!(fs::read(&registry).unwrap(), registry_before);
            assert!(!source.join("new").exists());
            assert_eq!(
                fs::read_to_string(source.join("authored.txt")).unwrap(),
                "preserve source\n"
            );
        }
    }

    #[cfg(unix)]
    {
        assert_eq!(
            fs::read_link(source.join("state-link")).unwrap(),
            fixture.child("outside")
        );
        assert_eq!(fs::read_dir(fixture.child("outside")).unwrap().count(), 0);
    }
    let case_alias = fixture.child("REPO/WORKTREE");
    let case_destination = case_alias.join("case-state");
    let case_alias_exists = case_alias.exists();
    let case_result = run_ocm(
        cwd,
        &env,
        &[
            "env",
            "create",
            "case-state",
            "--root",
            &path_string(&case_destination),
        ],
    );
    if case_alias_exists {
        assert!(!case_result.status.success());
        assert!(
            stderr(&case_result).contains("overlaps dev source"),
            "{}",
            stderr(&case_result)
        );
    } else {
        assert!(case_result.status.success(), "{}", stderr(&case_result));
    }

    // The originating repo, a source's sibling, and another env's root retain
    // their existing custom-root behavior; only registered source overlaps fail.
    for (name, destination) in [
        ("repo-state", fixture.child("repo/state")),
        ("source-sibling", fixture.child("repo/worktree-other")),
        ("nested-state", Path::new(&template.root).join("nested")),
    ] {
        let created = run_ocm(
            cwd,
            &env,
            &["env", "create", name, "--root", &path_string(&destination)],
        );
        assert!(created.status.success(), "{}", stderr(&created));
    }
    let cloned = run_ocm(cwd, &env, &["env", "clone", "template", "clone-ok"]);
    assert!(cloned.status.success(), "{}", stderr(&cloned));
    let imported = run_ocm(
        cwd,
        &env,
        &[
            "env",
            "import",
            &path_string(&archive),
            "--name",
            "import-ok",
        ],
    );
    assert!(imported.status.success(), "{}", stderr(&imported));
}
