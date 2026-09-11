mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Output;

use ocm::store::env_registry_path;

use support::{TestDir, ocm_env, path_string, run_ocm, stderr, write_text};

fn admit(
    fixture: &TestDir,
    env: &BTreeMap<String, String>,
    action: &str,
    name: &str,
    root: &Path,
) -> Output {
    let archive = path_string(&fixture.child("template.tar"));
    let root = path_string(root);
    let mut args = match action {
        "create" => vec!["env", "create", name, "--protect"],
        "clone" => vec!["env", "clone", "template", name],
        "import" => vec!["env", "import", &archive, "--name", name],
        _ => unreachable!(),
    };
    args.extend(["--root", &root]);
    run_ocm(fixture.path(), env, &args)
}

fn template(fixture: &TestDir, env: &BTreeMap<String, String>) {
    let created = run_ocm(fixture.path(), env, &["env", "create", "template"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let exported = run_ocm(
        fixture.path(),
        env,
        &[
            "env",
            "export",
            "template",
            "--output",
            &path_string(&fixture.child("template.tar")),
        ],
    );
    assert!(exported.status.success(), "{}", stderr(&exported));
}

#[test]
fn create_clone_and_import_reject_overlapping_roots_before_writing_state() {
    for protected in [false, true] {
        let fixture = TestDir::new("environment-root-isolation");
        let env = ocm_env(&fixture);
        let outer = fixture.child("roots/outer");
        let root = path_string(&outer);
        let mut args = vec!["env", "create", "outer", "--root", &root];
        if protected {
            args.push("--protect");
        }
        let created = run_ocm(fixture.path(), &env, &args);
        assert!(created.status.success(), "{}", stderr(&created));
        template(&fixture, &env);
        let marker = outer.join(".openclaw/workspace/keep.txt");
        write_text(&marker, "preserve existing environment\n");
        let empty = outer.join("empty");
        fs::create_dir(&empty).unwrap();
        let outside = fixture.child("outside");
        fs::create_dir(&outside).unwrap();

        let mut destinations = vec![
            outer.clone(),
            outer.parent().unwrap().to_path_buf(),
            outer.join("nested/new"),
            outer.join("unused/../new"),
            empty.clone(),
        ];
        #[cfg(unix)]
        {
            let alias = fixture.child("outer-alias");
            std::os::unix::fs::symlink(&outer, &alias).unwrap();
            destinations.push(alias.join("new"));
            let escape = outer.join("escape");
            std::os::unix::fs::symlink(&outside, &escape).unwrap();
            destinations.push(escape.clone());
            destinations.push(escape.join("new"));
        }
        #[cfg(target_os = "macos")]
        {
            let canonical = fs::canonicalize(&outer).unwrap();
            let data_alias =
                Path::new("/System/Volumes/Data").join(canonical.strip_prefix("/").unwrap());
            if data_alias.is_dir() {
                destinations.push(data_alias.join("new"));
            }
        }
        let case_alias = fixture.child("roots/OUTER");
        if case_alias.exists() {
            destinations.push(case_alias.join("new"));
        }

        let registry = env_registry_path(&env, fixture.path()).unwrap();
        let before = fs::read(&registry).unwrap();
        for destination in destinations {
            for action in ["create", "clone", "import"] {
                let rejected = admit(&fixture, &env, action, "blocked", &destination);
                assert!(!rejected.status.success(), "{action}: {destination:?}");
                assert!(
                    stderr(&rejected).contains("overlaps environment outer root"),
                    "{}",
                    stderr(&rejected)
                );
                assert_eq!(fs::read(&registry).unwrap(), before);
                assert_eq!(
                    fs::read_to_string(&marker).unwrap(),
                    "preserve existing environment\n"
                );
                assert!(!outer.join("nested").exists());
                assert!(!outer.join("unused").exists());
                assert!(!outer.join("new").exists());
                assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
                assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
            }
        }
        #[cfg(unix)]
        assert_eq!(fs::read_link(outer.join("escape")).unwrap(), outside);

        // A shared parent and a common name prefix do not imply overlapping roots.
        for action in ["create", "clone", "import"] {
            let name = format!("outer-{action}");
            let destination = outer.with_file_name(&name);
            let accepted = admit(&fixture, &env, action, &name, &destination);
            assert!(accepted.status.success(), "{}", stderr(&accepted));
            assert!(destination.join(".openclaw/workspace").is_dir());
        }
        if !case_alias.exists() {
            let accepted = admit(&fixture, &env, "create", "case-distinct", &case_alias);
            assert!(accepted.status.success(), "{}", stderr(&accepted));
        }
    }
}

#[test]
fn missing_registered_roots_still_reserve_their_locations() {
    let fixture = TestDir::new("missing-environment-root-isolation");
    let env = ocm_env(&fixture);
    template(&fixture, &env);
    let reserved = fixture.child("roots/reserved");
    let created = admit(&fixture, &env, "create", "reserved", &reserved);
    assert!(created.status.success(), "{}", stderr(&created));
    write_text(&reserved.join(".openclaw/workspace/keep.txt"), "retained\n");
    let displaced = fixture.child("displaced");
    fs::rename(&reserved, &displaced).unwrap();
    let registry = env_registry_path(&env, fixture.path()).unwrap();
    let before = fs::read(&registry).unwrap();

    for destination in [
        reserved.clone(),
        reserved.join("nested"),
        reserved.parent().unwrap().to_path_buf(),
    ] {
        for action in ["create", "clone", "import"] {
            let rejected = admit(&fixture, &env, action, "blocked", &destination);
            assert!(!rejected.status.success());
            assert!(
                stderr(&rejected).contains("overlaps environment reserved root"),
                "{}",
                stderr(&rejected)
            );
            assert_eq!(fs::read(&registry).unwrap(), before);
            assert!(!reserved.exists());
            assert_eq!(
                fs::read_to_string(displaced.join(".openclaw/workspace/keep.txt")).unwrap(),
                "retained\n"
            );
        }
    }
    let sibling = reserved.with_file_name("reserved-sibling");
    let accepted = admit(&fixture, &env, "create", "sibling", &sibling);
    assert!(accepted.status.success(), "{}", stderr(&accepted));
}

#[cfg(unix)]
#[test]
fn new_roots_cannot_contain_links_to_an_existing_environment() {
    let fixture = TestDir::new("environment-root-link-isolation");
    let env = ocm_env(&fixture);
    template(&fixture, &env);
    let parent = fixture.child("parent");
    let outside = fixture.child("outside");
    fs::create_dir(&parent).unwrap();
    fs::create_dir(&outside).unwrap();
    let link = parent.join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let created = admit(&fixture, &env, "create", "aliased", &link);
    assert!(created.status.success(), "{}", stderr(&created));
    let registry = env_registry_path(&env, fixture.path()).unwrap();
    let before = fs::read(&registry).unwrap();

    for action in ["create", "clone", "import"] {
        let rejected = admit(&fixture, &env, action, "blocked", &parent);
        assert!(!rejected.status.success());
        assert!(
            stderr(&rejected).contains("overlaps environment aliased root"),
            "{}",
            stderr(&rejected)
        );
        assert_eq!(fs::read(&registry).unwrap(), before);
        assert_eq!(fs::read_link(&link).unwrap(), outside);
        assert!(outside.join(".openclaw/workspace").is_dir());
        assert!(!parent.join(".openclaw").exists());
    }
}
