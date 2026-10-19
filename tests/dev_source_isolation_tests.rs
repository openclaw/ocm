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

#[test]
fn removal_preserves_shared_git_objects_but_allows_independent_clones() {
    for mode in ["shared", "copied", "linked"] {
        let fixture = TestDir::new(&format!("git-objects-{mode}"));
        let cwd = fixture.path();
        let env = ocm_env(&fixture);
        let donor_root = fixture.child("donor-env");
        create("donor", Some(&donor_root), None, &env, cwd).unwrap();
        let donor = init_checkout(&donor_root.join("checkout"));
        let source = fixture.child("borrowed-source");
        if mode == "linked" {
            git(
                &donor,
                &["worktree", "add", "--detach", &path_string(&source)],
            );
        } else {
            git(
                cwd,
                &[
                    "clone",
                    if mode == "shared" {
                        "--shared"
                    } else {
                        "--no-hardlinks"
                    },
                    &path_string(&donor),
                    &path_string(&source),
                ],
            );
        }
        let source = fs::canonicalize(source).unwrap();
        create("borrower", None, Some(borrowed(&source)), &env, cwd).unwrap();
        let registry = env_registry_path(&env, cwd).unwrap();
        let before = fs::read(&registry).unwrap();
        assert_eq!(git(&source, &["cat-file", "-t", "HEAD"]), "commit");
        git(&source, &["fsck", "--no-dangling"]);
        let removed = run_ocm(cwd, &env, &["env", "remove", "donor", "--force"]);
        if mode == "copied" {
            assert!(removed.status.success(), "{}", stderr(&removed));
            assert!(!donor_root.exists());
        } else {
            assert!(!removed.status.success());
            assert!(
                stderr(&removed).contains("registered dev source"),
                "{}",
                stderr(&removed)
            );
            assert!(donor_root.exists());
            assert_eq!(fs::read(&registry).unwrap(), before);
        }
        assert_eq!(git(&source, &["cat-file", "-t", "HEAD"]), "commit");
        git(&source, &["fsck", "--no-dangling"]);
    }
}

#[test]
fn removal_follows_relative_quoted_transitive_alternates_and_stops_cycles() {
    let fixture = TestDir::new("transitive-git-objects");
    let cwd = fixture.path();
    let env = ocm_env(&fixture);
    let donor_root = fixture.child("donor-env");
    create("donor", Some(&donor_root), None, &env, cwd).unwrap();
    let donor = init_checkout(&donor_root.join("checkout"));
    let middle = fixture.child("middle checkout");
    let source = fixture.child("borrowed-source");
    git(
        cwd,
        &[
            "clone",
            "--shared",
            &path_string(&donor),
            &path_string(&middle),
        ],
    );
    git(
        cwd,
        &[
            "clone",
            "--shared",
            &path_string(&middle),
            &path_string(&source),
        ],
    );
    write_text(
        &source.join(".git/objects/info/alternates"),
        "# Git object dependencies\n\n\"../../../middle\\040checkout/.git/objects\"\n",
    );
    // The donor owns the objects. Its backlink forms a cycle without changing
    // the borrower's need for the donor's object database.
    write_text(
        &donor.join(".git/objects/info/alternates"),
        &format!("{}\n", path_string(&source.join(".git/objects"))),
    );
    let source = fs::canonicalize(source).unwrap();
    create("borrower", None, Some(borrowed(&source)), &env, cwd).unwrap();
    git(&source, &["fsck", "--no-dangling"]);
    let removed = run_ocm(cwd, &env, &["env", "remove", "donor"]);
    assert!(!removed.status.success());
    assert!(
        stderr(&removed).contains("registered dev source"),
        "{}",
        stderr(&removed)
    );
    assert!(donor_root.exists());
    assert_eq!(git(&source, &["cat-file", "-t", "HEAD"]), "commit");
    git(&source, &["fsck", "--no-dangling"]);
}

#[cfg(unix)]
#[test]
fn removal_preserves_git_object_and_alternates_path_aliases() {
    use std::os::unix::fs::symlink;
    for mode in ["alternate-path", "alternate-file", "primary-objects"] {
        let fixture = TestDir::new(&format!("git-object-alias-{mode}"));
        let cwd = fixture.path();
        let env = ocm_env(&fixture);
        let donor_root = fixture.child("donor-env");
        create("donor", Some(&donor_root), None, &env, cwd).unwrap();
        let donor = init_checkout(&fixture.child("outside-donor"));
        let source = fixture.child("borrowed-source");
        git(
            cwd,
            &[
                "clone",
                "--shared",
                &path_string(&donor),
                &path_string(&source),
            ],
        );
        let objects = source.join(".git/objects");
        let alternates = objects.join("info/alternates");
        let alias = donor_root.join("dependency");
        match mode {
            "alternate-path" => {
                symlink(donor.join(".git/objects"), &alias).unwrap();
                write_text(&alternates, &format!("{}\n", path_string(&alias)));
            }
            "alternate-file" => {
                fs::rename(&alternates, &alias).unwrap();
                symlink(&alias, &alternates).unwrap();
            }
            _ => {
                fs::rename(&objects, &alias).unwrap();
                symlink(&alias, &objects).unwrap();
            }
        }
        let source = fs::canonicalize(source).unwrap();
        create("borrower", None, Some(borrowed(&source)), &env, cwd).unwrap();
        git(&source, &["fsck", "--no-dangling"]);
        let removed = run_ocm(cwd, &env, &["env", "remove", "donor"]);
        assert!(!removed.status.success(), "{mode}");
        assert!(
            stderr(&removed).contains("registered dev source"),
            "{mode}: {}",
            stderr(&removed)
        );
        assert!(donor_root.exists());
        assert_eq!(git(&source, &["cat-file", "-t", "HEAD"]), "commit");
        git(&source, &["fsck", "--no-dangling"]);
    }
}

#[test]
fn removal_preserves_known_alternates_when_metadata_is_missing_or_invalid() {
    for mode in ["missing", "directory", "oversized", "too-many"] {
        let fixture = TestDir::new(&format!("git-alternate-metadata-{mode}"));
        let cwd = fixture.path();
        let env = ocm_env(&fixture);
        let donor_root = fixture.child("donor-env");
        create("donor", Some(&donor_root), None, &env, cwd).unwrap();
        let donor = init_checkout(&donor_root.join("checkout"));
        let source = fixture.child("borrowed-source");
        git(
            cwd,
            &[
                "clone",
                "--shared",
                &path_string(&donor),
                &path_string(&source),
            ],
        );
        let source = fs::canonicalize(source).unwrap();
        create("borrower", None, Some(borrowed(&source)), &env, cwd).unwrap();
        create("unrelated", None, None, &env, cwd).unwrap();
        let alternates = source.join(".git/objects/info/alternates");
        let original = fs::read_to_string(&alternates).unwrap();
        match mode {
            "missing" => write_text(
                &alternates,
                &format!(
                    "{}\n{original}",
                    path_string(&fixture.child("absent-objects"))
                ),
            ),
            "directory" => {
                fs::remove_file(&alternates).unwrap();
                fs::create_dir(&alternates).unwrap();
            }
            "oversized" => write_text(&alternates, &"#".repeat(65537)),
            _ => write_text(&alternates, &".\n".repeat(1024)),
        }
        let registry = env_registry_path(&env, cwd).unwrap();
        let before = fs::read(&registry).unwrap();
        let removed = run_ocm(cwd, &env, &["env", "remove", "donor"]);
        assert!(!removed.status.success(), "{mode}");
        let expected = match mode {
            "missing" => "registered dev source",
            "too-many" => "too many alternate paths",
            _ => "invalid Git alternates file",
        };
        assert!(
            stderr(&removed).contains(expected),
            "{mode}: {}",
            stderr(&removed)
        );
        if mode == "too-many" {
            assert!(stderr(&removed).contains(&path_string(&alternates)));
            assert!(stderr(&removed).contains("preserving all required Git objects, then retry"));
        }
        assert!(donor_root.exists());
        assert_eq!(fs::read(&registry).unwrap(), before);
        if mode == "missing" {
            // Missing unrelated storage remains reserved but does not block a
            // disjoint removal or hide the surviving donor dependency.
            let removed = run_ocm(cwd, &env, &["env", "remove", "unrelated"]);
            assert!(removed.status.success(), "{}", stderr(&removed));
            assert_eq!(git(&source, &["cat-file", "-t", "HEAD"]), "commit");
        }
    }
}

#[test]
fn removal_preserves_initialized_submodule_object_dependencies() {
    for mode in [
        "shared",
        "nested",
        "old-form",
        "copied",
        "index-fallback",
        "head-fallback",
    ] {
        let fixture = TestDir::new(&format!("submodule-object-dependency-{mode}"));
        let cwd = fixture.path();
        let env = ocm_env(&fixture);
        let donor_root = fixture.child("donor-env");
        create("donor", Some(&donor_root), None, &env, cwd).unwrap();
        let donor = init_checkout(&donor_root.join("checkout"));
        let source = init_checkout(&fixture.child("source"));
        let add = |parent: &Path, repository: &Path, reference: bool| {
            let url = url::Url::from_file_path(repository).unwrap().to_string();
            let repository = path_string(repository);
            let mut command = vec!["-c", "protocol.file.allow=always", "submodule", "add"];
            if reference {
                command.extend(["--reference", &repository]);
            }
            command.extend([&url, "dependency"]);
            git(parent, &command);
            parent.join("dependency")
        };
        let parent = if mode == "nested" {
            let middle = init_checkout(&fixture.child("middle"));
            add(&source, &middle, false)
        } else {
            source.clone()
        };
        let module = add(&parent, &donor, mode != "copied");
        if mode.ends_with("fallback") {
            git(
                &source,
                &[
                    "-c",
                    "user.name=OCM Tests",
                    "-c",
                    "user.email=tests@example.com",
                    "commit",
                    "-m",
                    "submodule",
                ],
            );
            fs::remove_file(source.join(".gitmodules")).unwrap();
            if mode == "head-fallback" {
                git(&source, &["rm", "--cached", ".gitmodules"]);
            }
            assert!(git(&source, &["submodule", "status"]).contains("dependency"));
        }
        let private = PathBuf::from(git(&module, &["rev-parse", "--absolute-git-dir"]));
        assert_eq!(
            private.join("objects/info/alternates").is_file(),
            mode != "copied"
        );
        if mode == "old-form" {
            // Git also supports a submodule with its own .git directory.
            git(
                &source,
                &[
                    "config",
                    "--file",
                    &path_string(&private.join("config")),
                    "--unset",
                    "core.worktree",
                ],
            );
            fs::remove_file(module.join(".git")).unwrap();
            fs::rename(&private, module.join(".git")).unwrap();
        }
        create("borrower", None, Some(borrowed(&source)), &env, cwd).unwrap();
        let registry = env_registry_path(&env, cwd).unwrap();
        let before = fs::read(&registry).unwrap();
        assert_eq!(git(&module, &["cat-file", "-t", "HEAD"]), "commit");
        let removed = run_ocm(cwd, &env, &["env", "remove", "donor", "--force"]);
        assert_eq!(
            removed.status.success(),
            mode == "copied",
            "{mode}: {}",
            stderr(&removed)
        );
        assert_eq!(donor_root.exists(), mode != "copied");
        if mode != "copied" {
            assert!(
                stderr(&removed).contains("registered dev source"),
                "{}",
                stderr(&removed)
            );
            assert_eq!(fs::read(&registry).unwrap(), before);
        }
        assert_eq!(git(&module, &["cat-file", "-t", "HEAD"]), "commit");
        git(&module, &["fsck", "--no-dangling"]);
    }
}

#[test]
fn removal_refuses_uninspectable_submodule_metadata() {
    for contents in [
        "[invalid".to_string(),
        "#".repeat(65537),
        "[submodule \"unsafe\"]\npath = ../outside\n".to_string(),
        (0..1024)
            .map(|index| format!("[submodule \"{index}\"]\npath = module\n"))
            .collect::<String>(),
    ] {
        let fixture = TestDir::new("invalid-submodule-metadata");
        let cwd = fixture.path();
        let env = ocm_env(&fixture);
        let donor_root = fixture.child("donor-env");
        create("donor", Some(&donor_root), None, &env, cwd).unwrap();
        let source = init_checkout(&fixture.child("source"));
        create("borrower", None, Some(borrowed(&source)), &env, cwd).unwrap();
        let excessive = contents.contains("path = module");
        if excessive {
            init_checkout(&source.join("module"));
        }
        let modules = source.join(".gitmodules");
        write_text(&modules, &contents);
        let registry = env_registry_path(&env, cwd).unwrap();
        let before = fs::read(&registry).unwrap();
        let removed = run_ocm(cwd, &env, &["env", "remove", "donor", "--force"]);
        assert!(!removed.status.success());
        if excessive {
            assert!(stderr(&removed).contains("too many submodule paths"));
            assert!(stderr(&removed).contains(&path_string(&modules)));
            assert!(stderr(&removed).contains("preserving all required Git objects, then retry"));
        } else {
            assert!(
                stderr(&removed).contains("Git submodule"),
                "{}",
                stderr(&removed)
            );
        }
        assert!(donor_root.exists());
        assert_eq!(fs::read(&registry).unwrap(), before);
    }
}
