mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ocm::env::{CreateEnvironmentOptions, EnvDevMeta, EnvMeta};
use ocm::store::{create_environment, env_registry_path, save_environment};

use support::{TestDir, ocm_env, path_string, run_ocm, stderr, stdout, write_text};

fn create(
    name: &str,
    root: Option<&Path>,
    dev: Option<EnvDevMeta>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
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
}

fn dev_binding(source: &Path) -> EnvDevMeta {
    EnvDevMeta::Owned {
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
    create("developer", None, Some(dev_binding(&source)), &env, cwd).unwrap();
    let template = create("template", None, None, &env, cwd).unwrap();
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

    let nested = Path::new(&template.root).join("nested");
    let rejected = create("nested-state", Some(&nested), None, &env, cwd).unwrap_err();
    assert!(
        rejected.contains("overlaps environment template root"),
        "{rejected}"
    );
    assert!(!nested.exists());

    // Separate roots in the originating repo and beside a source remain valid.
    for (name, destination) in [
        ("repo-state", fixture.child("repo/state")),
        ("source-sibling", fixture.child("repo/worktree-other")),
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

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    stdout(&output).trim().to_string()
}

fn init_checkout(path: &Path) -> PathBuf {
    write_text(&path.join("package.json"), r#"{"name":"openclaw"}"#);
    write_text(&path.join("scripts/run-node.mjs"), "// retained source\n");
    git(path, &["init"]);
    git(path, &["add", "."]);
    git(
        path,
        &[
            "-c",
            "user.name=OCM Tests",
            "-c",
            "user.email=tests@example.com",
            "commit",
            "-m",
            "init",
        ],
    );
    fs::canonicalize(path).unwrap()
}

fn borrowed(source: &Path) -> EnvDevMeta {
    EnvDevMeta::Borrowed {
        source_root: path_string(source),
    }
}

#[test]
fn missing_unicode_borrowed_sources_preserve_aliases_and_allow_siblings() {
    let fixture = TestDir::new("missing-unicode-borrowed-source");
    let cwd = fixture.path();
    let env = ocm_env(&fixture);
    let source = init_checkout(&fixture.child("projects/café-source"));
    create("developer", None, Some(borrowed(&source)), &env, cwd).unwrap();
    let retained = fixture.child("retained");
    fs::rename(&source, &retained).unwrap();
    let registry = env_registry_path(&env, cwd).unwrap();
    let before = fs::read(&registry).unwrap();

    for destination in [
        source.clone(),
        source.with_file_name("CAFE\u{301}-SOURCE").join("state"),
    ] {
        let error = create("blocked", Some(&destination), None, &env, cwd).unwrap_err();
        assert!(error.contains("overlaps borrowed source"), "{error}");
        assert_eq!(fs::read(&registry).unwrap(), before);
        assert!(!destination.exists());
    }
    let sibling = source.with_file_name("日本語の環境ルート");
    create("sibling", Some(&sibling), None, &env, cwd).unwrap();
    assert!(sibling.join(".openclaw/workspace").is_dir());
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(retained.join("scripts/run-node.mjs")).unwrap(),
        "// retained source\n"
    );
}

#[test]
fn missing_borrowed_sources_remain_reserved_until_the_binding_is_removed() {
    let fixture = TestDir::new("missing-borrowed-reservations");
    let cwd = fixture.path();
    let env = ocm_env(&fixture);
    let source = init_checkout(&fixture.child("projects/openclaw"));
    let mut developer = create("developer", None, Some(borrowed(&source)), &env, cwd).unwrap();
    let template = create("template", None, None, &env, cwd).unwrap();
    let archive = fixture.child("template.tar");
    let exported = run_ocm(
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
    assert!(exported.status.success(), "{}", stderr(&exported));
    let retained = fixture.child("retained");
    fs::rename(&source, &retained).unwrap();
    // Recovery writes retain an unchanged binding while its source is displaced.
    developer.protected = true;
    save_environment(developer, &env, cwd).unwrap();
    let registry = env_registry_path(&env, cwd).unwrap();
    let before = fs::read(&registry).unwrap();
    let mut destinations = vec![
        source.clone(),
        source.join("new/state"),
        source.parent().unwrap().to_path_buf(),
        source.with_file_name("OPENCLAW").join("state"),
    ];
    #[cfg(unix)]
    {
        let alias = fixture.child("projects-alias");
        std::os::unix::fs::symlink(source.parent().unwrap(), &alias).unwrap();
        destinations.push(alias.join("openclaw/state"));
    }
    for destination in &destinations {
        let destination = path_string(destination);
        let archive = path_string(&archive);
        for action in ["create", "clone", "import"] {
            let mut args = match action {
                "create" => vec!["env", "create", "blocked"],
                "clone" => vec!["env", "clone", "template", "blocked"],
                _ => vec!["env", "import", &archive, "--name", "blocked"],
            };
            args.extend(["--root", &destination]);
            let rejected = run_ocm(cwd, &env, &args);
            assert!(!rejected.status.success());
            assert!(
                stderr(&rejected).contains("overlaps borrowed source"),
                "{}",
                stderr(&rejected)
            );
            assert_eq!(fs::read(&registry).unwrap(), before);
            assert!(!source.exists());
        }
    }
    let nested = Path::new(&template.root).join("nested");
    let rejected = create("nested", Some(&nested), None, &env, cwd).unwrap_err();
    assert!(
        rejected.contains("overlaps environment template root"),
        "{rejected}"
    );
    assert_eq!(fs::read(&registry).unwrap(), before);
    assert!(!nested.exists());
    create(
        "sibling",
        Some(&source.with_file_name("sibling")),
        None,
        &env,
        cwd,
    )
    .unwrap();
    let removed = run_ocm(cwd, &env, &["env", "remove", "developer", "--force"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    create("reused", Some(&source), None, &env, cwd).unwrap();
    assert!(!source.join(".git").exists());
    assert_eq!(
        fs::read_to_string(retained.join("scripts/run-node.mjs")).unwrap(),
        "// retained source\n"
    );

    for dangling_link in [false, true] {
        #[cfg(not(unix))]
        if dangling_link {
            continue;
        }
        let fixture = TestDir::new("missing-borrowed-git-target");
        let cwd = fixture.path();
        let env = ocm_env(&fixture);
        let source = init_checkout(&fixture.child("openclaw"));
        let metadata = fixture.child("metadata");
        git(
            &source,
            &["init", "--separate-git-dir", &path_string(&metadata)],
        );
        let git_entry = source.join(".git");
        #[cfg(unix)]
        if dangling_link {
            fs::remove_file(&git_entry).unwrap();
            std::os::unix::fs::symlink(&metadata, &git_entry).unwrap();
        }
        create("developer", None, Some(borrowed(&source)), &env, cwd).unwrap();
        let registry = env_registry_path(&env, cwd).unwrap();
        let before = fs::read(&registry).unwrap();
        if !dangling_link {
            let pointer = fs::read(&git_entry).unwrap();
            fs::write(&git_entry, "malformed Git pointer\n").unwrap();
            let rejected = create("invalid", None, None, &env, cwd).unwrap_err();
            assert!(
                rejected.contains("invalid Git identity pointer"),
                "{rejected}"
            );
            assert_eq!(fs::read(&registry).unwrap(), before);
            fs::write(&git_entry, pointer).unwrap();
        }
        let retained = fixture.child("retained-git");
        let head = fs::read(metadata.join("HEAD")).unwrap();
        fs::rename(&metadata, &retained).unwrap();
        for destination in [metadata.clone(), metadata.join("new-state")] {
            let rejected = create("blocked", Some(&destination), None, &env, cwd).unwrap_err();
            assert!(rejected.contains("overlaps borrowed source"), "{rejected}");
            assert_eq!(fs::read(&registry).unwrap(), before);
            assert!(!metadata.exists());
        }
        create("unrelated", None, None, &env, cwd).unwrap();
        let removed = run_ocm(cwd, &env, &["env", "remove", "developer"]);
        assert!(removed.status.success(), "{}", stderr(&removed));
        assert!(!metadata.exists());
        assert!(fs::symlink_metadata(&git_entry).is_ok());
        assert_eq!(fs::read(retained.join("HEAD")).unwrap(), head);
        assert_eq!(
            fs::read_to_string(source.join("scripts/run-node.mjs")).unwrap(),
            "// retained source\n"
        );
        create("reused", Some(&metadata), None, &env, cwd).unwrap();
    }
}

#[test]
fn borrowed_state_roots_cannot_overlap_the_source_or_its_git_metadata() {
    let fixture = TestDir::new("borrowed-source-state-isolation");
    let cwd = fixture.path();
    let env = ocm_env(&fixture);
    let repo = init_checkout(&fixture.child("openclaw"));
    git(&repo, &["worktree", "add", "--detach", "../linked"]);
    let linked = fs::canonicalize(fixture.child("linked")).unwrap();
    // Another existing state root inside the selected repo is still supported.
    create("contained", Some(&repo.join("state")), None, &env, cwd).unwrap();
    let registry = env_registry_path(&env, cwd).unwrap();
    let before = fs::read(&registry).unwrap();
    for source in [&repo, &linked] {
        let common = PathBuf::from(git(
            source,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ));
        let private = PathBuf::from(git(source, &["rev-parse", "--absolute-git-dir"]));
        for destination in [
            source.to_path_buf(),
            source.join("new-state"),
            common.join("new-state"),
            private.join("new-state"),
        ] {
            let rejected = create(
                "blocked",
                Some(&destination),
                Some(borrowed(source)),
                &env,
                cwd,
            )
            .unwrap_err();
            assert!(rejected.contains("overlaps borrowed source"), "{rejected}");
            assert_eq!(fs::read(&registry).unwrap(), before);
            if destination != *source {
                assert!(!destination.exists());
            }
        }
    }
    let mut developer = create("developer", None, Some(borrowed(&linked)), &env, cwd).unwrap();
    let before = fs::read(&registry).unwrap();
    developer.root = path_string(&repo.join(".git/new-state"));
    let rejected = save_environment(developer, &env, cwd).unwrap_err();
    assert!(rejected.contains("overlaps borrowed source"), "{rejected}");
    assert_eq!(fs::read(&registry).unwrap(), before);
    assert!(!repo.join(".git/new-state").exists());
    create("main-source", None, Some(borrowed(&repo)), &env, cwd).unwrap();
}
