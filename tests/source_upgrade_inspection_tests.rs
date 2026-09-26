mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use support::{TestDir, ocm_env, path_string, run_ocm, stderr, stdout, write_text};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn fixture(root: &TestDir) -> (BTreeMap<String, String>, String) {
    let repo = root.child("source");
    fs::create_dir_all(&repo).unwrap();
    write_text(
        &repo.join("package.json"),
        r#"{"name":"openclaw","version":"2026.9.3","scripts":{"openclaw":"node scripts/run-node.mjs"}}"#,
    );
    // Inspection must never run either source entry point.
    write_text(
        &repo.join("openclaw.mjs"),
        "throw new Error('launcher executed');\n",
    );
    write_text(
        &repo.join("scripts/run-node.mjs"),
        "throw new Error('source executed');\n",
    );
    write_text(&repo.join(".gitignore"), "dist/\n");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "source"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/never-contact",
        ],
    );
    git(&repo, &["update-ref", "refs/remotes/origin/main", &head]);
    git(&repo, &["config", "branch.main.remote", "origin"]);
    git(&repo, &["config", "branch.main.merge", "refs/heads/main"]);
    write_text(
        &repo.join("dist/build-info.json"),
        &json!({"version":"2026.9.3","commit":head}).to_string(),
    );
    let mut env = ocm_env(root);
    env.insert("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string());
    for args in [
        vec![
            "launcher",
            "add",
            "source",
            "--command",
            "pnpm openclaw",
            "--cwd",
            repo.to_str().unwrap(),
        ],
        vec!["env", "create", "demo", "--launcher", "source"],
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    (env, head)
}

fn inspect(root: &TestDir, env: &BTreeMap<String, String>) -> Value {
    let output = run_ocm(
        root.path(),
        env,
        &["upgrade", "demo", "--dry-run", "--json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["outcome"], "local-command");
    assert_eq!(value["bindingKind"], "launcher");
    assert_eq!(value["bindingName"], "source");
    assert!(value["serviceAction"].is_null());
    assert!(value["snapshotId"].is_null());
    value
}

#[test]
fn source_upgrade_reports_built_identity_without_executing_or_mutating_source() {
    let root = TestDir::new("source-upgrade-identity");
    let (env, built) = fixture(&root);
    let repo = root.child("source");
    let index = fs::read(repo.join(".git/index")).unwrap();
    let before_env = run_ocm(root.path(), &env, &["env", "show", "demo", "--json"]);
    let current = inspect(&root, &env);
    assert_eq!(current["source"]["head"], built);
    assert_eq!(current["source"]["builtCommit"], built);
    assert_eq!(current["source"]["buildMatchesHead"], true);
    assert_eq!(current["source"]["workingTreeClean"], true);
    assert_eq!(current["source"]["trackingRef"], "refs/remotes/origin/main");
    assert_eq!(current["source"]["trackingHead"], built);
    assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
    git(&repo, &["commit", "--allow-empty", "-m", "pulled source"]);
    let changed = inspect(&root, &env);
    assert_eq!(changed["source"]["buildMatchesHead"], false);
    assert_eq!(changed["source"]["builtCommit"], built);
    assert_eq!(
        changed["source"]["head"],
        git(&repo, &["rev-parse", "HEAD"])
    );
    let raw = run_ocm(
        root.path(),
        &env,
        &["upgrade", "demo", "--dry-run", "--raw"],
    );
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert!(stdout(&raw).contains("buildMatchesHead=false"));
    let batch = run_ocm(
        root.path(),
        &env,
        &["upgrade", "--all", "--dry-run", "--json"],
    );
    let batch: Value = serde_json::from_str(&stdout(&batch)).unwrap();
    assert_eq!(batch["skipped"], 1);
    assert_eq!(batch["changed"], 0);
    assert_eq!(batch["results"][0]["source"]["buildMatchesHead"], false);
    let history = run_ocm(root.path(), &env, &["upgrade", "history", "demo", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&history)).unwrap(),
        json!([])
    );
    let after_env = run_ocm(root.path(), &env, &["env", "show", "demo", "--json"]);
    assert_eq!(stdout(&before_env), stdout(&after_env));
}

#[test]
fn source_upgrade_keeps_missing_and_invalid_build_identity_unknown() {
    let root = TestDir::new("source-upgrade-unknown");
    let (env, _) = fixture(&root);
    let info = root.child("source/dist/build-info.json");
    for contents in [
        None,
        Some("invalid"),
        Some(r#"{"version":"2026.9.3","commit":"short"}"#),
    ] {
        if let Some(contents) = contents {
            fs::write(&info, contents).unwrap();
        } else {
            fs::remove_file(&info).unwrap();
        }
        let result = inspect(&root, &env);
        assert!(result["source"]["builtCommit"].is_null());
        assert!(result["source"]["buildMatchesHead"].is_null());
        assert!(!result["source"]["issues"].as_array().unwrap().is_empty());
    }
    git(&root.child("source"), &["checkout", "--detach"]);
    write_text(&root.child("source/operator-notes.txt"), "preserve\n");
    let result = inspect(&root, &env);
    assert_eq!(result["source"]["workingTreeClean"], false);
    assert!(result["source"]["trackingRef"].is_null());
}

#[cfg(unix)]
#[test]
fn source_upgrade_reports_shared_aliases_and_refuses_external_build_metadata() {
    let root = TestDir::new("source-upgrade-alias");
    let (env, _) = fixture(&root);
    let alias = root.child("alias");
    std::os::unix::fs::symlink(root.child("source"), &alias).unwrap();
    let output = run_ocm(
        root.path(),
        &env,
        &[
            "launcher",
            "add",
            "alias",
            "--command",
            "pnpm openclaw",
            "--cwd",
            &path_string(&alias),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let output = run_ocm(
        root.path(),
        &env,
        &["env", "create", "other", "--launcher", "alias"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let result = inspect(&root, &env);
    assert_eq!(result["source"]["sharedEnvironments"], json!(["other"]));
    let info = root.child("source/dist/build-info.json");
    fs::rename(&info, root.child("outside.json")).unwrap();
    std::os::unix::fs::symlink(root.child("outside.json"), &info).unwrap();
    let result = inspect(&root, &env);
    assert!(result["source"]["builtCommit"].is_null());
    assert!(
        result["source"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue.as_str().unwrap().contains("outside the checkout"))
    );
}

#[test]
fn source_upgrade_recognizes_direct_node_but_does_not_infer_shell_wrappers() {
    let root = TestDir::new("source-upgrade-launcher");
    let (env, _) = fixture(&root);
    let repo = path_string(&root.child("source"));
    let absolute = format!("node {}", path_string(&root.child("source/openclaw.mjs")));
    for (name, command, with_cwd, recognized) in [
        ("direct", "node openclaw.mjs", true, true),
        ("absolute", absolute.as_str(), false, true),
        ("relative", "node openclaw.mjs", false, false),
        ("opaque", "echo wrapper && pnpm openclaw", true, false),
    ] {
        let mut args = vec!["launcher", "add", name, "--command", command];
        if with_cwd {
            args.extend(["--cwd", &repo]);
        }
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
        let output = run_ocm(root.path(), &env, &["env", "set-launcher", "demo", name]);
        assert!(output.status.success(), "{}", stderr(&output));
        let output = run_ocm(
            root.path(),
            &env,
            &["upgrade", "demo", "--dry-run", "--json"],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(result.get("source").is_some(), recognized);
        assert_eq!(result["outcome"], "local-command");
    }
}

#[cfg(unix)]
#[test]
fn source_inspection_recognizes_quoted_node_aliases_without_executing_them() {
    let root = TestDir::new("source-upgrade-quoted");
    let (env, head) = fixture(&root);
    let repo = root.child("source");
    let alias = root.child("Source With Spaces");
    std::os::unix::fs::symlink(&repo, &alias).unwrap();
    let entry = path_string(&alias.join("openclaw.mjs"));
    let symbols = root.child("Source #1 & Data");
    std::os::unix::fs::symlink(&repo, &symbols).unwrap();
    let symbols_entry = path_string(&symbols.join("openclaw.mjs"));
    let literal = root.child("Literal $(touch expanded) `touch expanded`");
    std::os::unix::fs::symlink(&repo, &literal).unwrap();
    let literal_entry = path_string(&literal.join("openclaw.mjs"));
    let marker = root.child("launcher-executed");
    write_text(
        &repo.join("openclaw.mjs"),
        &format!(
            "import fs from 'node:fs'; fs.writeFileSync({}, 'executed'); console.log('quoted-fixture-version');\n",
            json!(path_string(&marker))
        ),
    );
    let mut recognized_names = Vec::new();
    for (name, command, recognized) in [
        ("single", format!("node '{entry}'"), true),
        ("double", format!("\"node\" \"{entry}\""), true),
        ("symbols-single", format!("node '{symbols_entry}'"), true),
        ("symbols-double", format!("node \"{symbols_entry}\""), true),
        ("literal-single", format!("node '{literal_entry}'"), true),
        // This path exists literally, so rejecting it cannot rely on a missing file.
        (
            "literal-expanded",
            format!("node \"{literal_entry}\""),
            false,
        ),
        ("variable", "node \"$ENTRY\"".to_string(), false),
        (
            "substitution",
            format!("node \"$(touch {})\"", path_string(&marker)),
            false,
        ),
        ("pipeline", format!("node '{entry}' | cat"), false),
        ("builtin", format!("exec \"node\" '{entry}'"), false),
    ] {
        let output = run_ocm(
            root.path(),
            &env,
            &["launcher", "add", name, "--command", &command],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let output = run_ocm(
            root.path(),
            &env,
            &["env", "create", name, "--launcher", name],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let output = run_ocm(root.path(), &env, &["upgrade", name, "--dry-run", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(result["outcome"], "local-command");
        if recognized {
            recognized_names.push(name);
            assert_eq!(
                result["source"]["root"],
                path_string(&fs::canonicalize(&repo).unwrap())
            );
            assert_eq!(result["source"]["head"], head);
            assert!(
                result["source"]["sharedEnvironments"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("demo"))
            );
        } else {
            assert!(result.get("source").is_none(), "{name}: {result}");
        }
    }
    recognized_names.sort();
    assert_eq!(
        inspect(&root, &env)["source"]["sharedEnvironments"],
        json!(recognized_names)
    );
    assert!(
        !marker.exists(),
        "inspection executed a launcher or substitution"
    );
    assert!(!root.child("expanded").exists());
    // The same literal launchers must remain executable through the real command path.
    for name in recognized_names {
        let output = run_ocm(root.path(), &env, &["env", "run", name, "--", "--version"]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(stdout(&output).trim(), "quoted-fixture-version");
        assert!(marker.exists());
        fs::remove_file(&marker).unwrap();
    }
    assert!(
        !root.child("expanded").exists(),
        "single-quoted data expanded"
    );
}

#[cfg(unix)]
#[test]
fn source_inspection_does_not_execute_git_filters() {
    let root = TestDir::new("source-upgrade-filter");
    let (env, _) = fixture(&root);
    let repo = root.child("source");
    write_text(&repo.join(".gitattributes"), "openclaw.mjs filter=probe\n");
    git(&repo, &["add", ".gitattributes"]);
    git(&repo, &["commit", "-m", "attributes"]);
    git(
        &repo,
        &["config", "filter.probe.clean", "touch filter-executed; cat"],
    );
    write_text(
        &repo.join("openclaw.mjs"),
        "throw new Error('filtered executed');\n",
    );
    let result = inspect(&root, &env);
    assert!(
        !repo.join("filter-executed").exists(),
        "inspection executed a Git filter"
    );
    assert!(result["source"]["workingTreeClean"].is_null());
}

#[test]
fn source_inspection_does_not_fetch_missing_rename_objects() {
    let root = TestDir::new("source-upgrade-partial-clone");
    let (mut env, _) = fixture(&root);
    let repo = root.child("source");
    let content = (0..300)
        .map(|line| format!("fixture line {line:05} common content\n"))
        .collect::<String>();
    write_text(&repo.join("docs/hidden.txt"), &content);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "hidden blob"]);
    let blob = git(&repo, &["rev-parse", "HEAD:docs/hidden.txt"]);
    let seed = root.child("seed");
    fs::rename(&repo, &seed).unwrap();
    git(&seed, &["config", "uploadpack.allowFilter", "true"]);
    let origin = url::Url::from_directory_path(&seed).unwrap().to_string();
    git(
        root.path(),
        &[
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &origin,
            repo.to_str().unwrap(),
        ],
    );
    git(
        &repo,
        &[
            "sparse-checkout",
            "set",
            "--no-cone",
            "/package.json",
            "/openclaw.mjs",
            "/scripts/",
        ],
    );
    git(&repo, &["checkout", "main"]);
    git(&repo, &["rm", "--cached", "--sparse", "docs/hidden.txt"]);
    write_text(
        &repo.join("replacement.txt"),
        &content.replace("fixture line 00000", "changed line 00000"),
    );
    git(&repo, &["add", "--sparse", "replacement.txt"]);
    // Enumerating local objects does not fetch the missing blob to check it.
    let objects = git(
        &repo,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
    );
    assert!(!objects.lines().any(|oid| oid == blob));
    let index = fs::read(repo.join(".git/index")).unwrap();
    let before_env = run_ocm(root.path(), &env, &["env", "show", "demo", "--json"]);
    let trace = root.child("git-trace.jsonl");
    env.insert("GIT_TRACE2_EVENT".to_string(), path_string(&trace));
    // The inspector must override callers that explicitly allow lazy fetching.
    env.insert("GIT_NO_LAZY_FETCH".to_string(), "0".to_string());
    let result = inspect(&root, &env);
    assert!(result["source"]["workingTreeClean"].is_null());
    assert_eq!(result["source"]["head"], git(&repo, &["rev-parse", "HEAD"]));
    assert_eq!(result["source"]["trackingRef"], "refs/remotes/origin/main");
    assert!(!result["source"]["issues"].as_array().unwrap().is_empty());
    assert_eq!(
        git(
            &repo,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)"
            ],
        ),
        objects
    );
    assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
    for line in fs::read_to_string(&trace).unwrap().lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        if event["event"] == "child_start" {
            assert!(
                !event["argv"].as_array().unwrap().iter().any(|arg| {
                    arg.as_str()
                        .is_some_and(|arg| arg == "fetch" || arg.contains("upload-pack"))
                }),
                "inspection started a fetch: {event}"
            );
        }
    }
    let after_env = run_ocm(root.path(), &env, &["env", "show", "demo", "--json"]);
    assert_eq!(stdout(&before_env), stdout(&after_env));
    let history = run_ocm(root.path(), &env, &["upgrade", "history", "demo", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&history)).unwrap(),
        json!([])
    );
}

#[test]
fn source_inspection_recognizes_all_promisor_configuration_forms() {
    let root = TestDir::new("source-upgrade-promisor-config");
    let (mut env, head) = fixture(&root);
    let config = root.child("gitconfig");
    env.insert("GIT_CONFIG_GLOBAL".to_string(), path_string(&config));
    let unrelated_config = root.child("unrelated-gitconfig");
    write_text(&unrelated_config, "");
    env.insert("GIT_CONFIG".to_string(), path_string(&unrelated_config));
    for contents in [
        "[extensions]\npartialClone = origin\n",
        "[remote \"another\"]\npromisor = true\n",
        "[remote \"another\"]\npartialCloneFilter = blob:none\n",
    ] {
        write_text(&config, contents);
        let result = inspect(&root, &env);
        assert_eq!(result["source"]["head"], head);
        assert!(result["source"]["workingTreeClean"].is_null());
        assert!(
            result["source"]["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| { issue.as_str().unwrap().contains("fetching missing objects") })
        );
    }
    write_text(&config, "");
    assert_eq!(inspect(&root, &env)["source"]["workingTreeClean"], true);
}

#[cfg(unix)]
#[test]
fn source_inspection_rejects_non_regular_metadata_and_submodule_status() {
    let root = TestDir::new("source-upgrade-special-files");
    let (env, head) = fixture(&root);
    let repo = root.child("source");
    let info = repo.join("dist/build-info.json");
    fs::remove_file(&info).unwrap();
    let output = Command::new("mkfifo").arg(&info).output().unwrap();
    assert!(output.status.success());
    let result = inspect(&root, &env);
    assert!(result["source"]["builtCommit"].is_null());
    assert!(
        result["source"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue.as_str().unwrap().contains("not a regular file"))
    );
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},nested"),
        ],
    );
    let result = inspect(&root, &env);
    assert!(result["source"]["workingTreeClean"].is_null());
    assert!(
        result["source"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue.as_str().unwrap().contains("submodules"))
    );
}

#[test]
fn source_inspection_reports_staged_gitlink_removal_and_replacements() {
    let root = TestDir::new("source-upgrade-staged-gitlink");
    let (mut env, _) = fixture(&root);
    let repo = root.child("source");
    let nested = repo.join("nested");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-b", "main"]);
    write_text(&nested.join("tracked"), "nested content\n");
    git(&nested, &["add", "."]);
    git(&nested, &["commit", "-m", "nested source"]);
    let nested_head = git(&nested, &["rev-parse", "HEAD"]);
    write_text(&repo.join(".gitignore"), "dist/\nnested/\n");
    write_text(
        &repo.join(".gitmodules"),
        "[submodule \"nested\"]\npath = nested\nurl = https://example.invalid/nested\nignore = all\n",
    );
    git(&repo, &["add", ".gitignore", ".gitmodules"]);
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{nested_head},nested"),
        ],
    );
    // Both a staged addition and an unchanged committed submodule stay unknown.
    assert!(inspect(&root, &env)["source"]["workingTreeClean"].is_null());
    git(&repo, &["commit", "-m", "record submodule"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    write_text(
        &repo.join("dist/build-info.json"),
        &json!({"version":"2026.9.3","commit":head}).to_string(),
    );
    git(&repo, &["config", "submodule.nested.ignore", "all"]);
    git(&repo, &["config", "status.submoduleSummary", "true"]);
    assert!(inspect(&root, &env)["source"]["workingTreeClean"].is_null());
    write_text(&nested.join("tracked"), "modified nested content\n");
    let trace = root.child("gitlink-trace.jsonl");
    env.insert("GIT_TRACE2_EVENT".to_string(), path_string(&trace));
    git(&repo, &["update-index", "--force-remove", "nested"]);
    for replacement in [false, true] {
        if replacement {
            fs::remove_dir_all(&nested).unwrap();
            write_text(&nested, "regular file replacement\n");
            git(&repo, &["add", "--force", "nested"]);
        }
        let index = fs::read(repo.join(".git/index")).unwrap();
        let before_env = run_ocm(root.path(), &env, &["env", "show", "demo", "--json"]);
        let expected = inspect(&root, &env);
        assert_eq!(expected["source"]["workingTreeClean"], false);
        assert!(
            expected["source"]["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| {
                    issue
                        .as_str()
                        .unwrap()
                        .contains("modified or untracked files")
                })
        );
        let normal = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        let batch = run_ocm(root.path(), &env, &["upgrade", "--all", "--json"]);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(!normal.status.success());
            assert!(stderr(&normal).contains("cannot update source"));
            assert!(!batch.status.success());
            let batch: Value = serde_json::from_str(&stdout(&batch)).unwrap();
            assert_eq!(batch["failed"], 1);
            assert_eq!(batch["results"][0]["outcome"], "failed");
            assert!(batch["results"][0]["snapshotId"].is_null());
        } else {
            assert!(normal.status.success(), "{}", stderr(&normal));
            assert_eq!(
                serde_json::from_str::<Value>(&stdout(&normal)).unwrap(),
                expected
            );
            assert!(batch.status.success(), "{}", stderr(&batch));
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let job = source_job_refusal(&root, &env);
            assert!(
                job["error"]
                    .as_str()
                    .unwrap()
                    .contains("cannot update source")
            );
            assert!(job["result"].is_null());
        }
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
        let after_env = run_ocm(root.path(), &env, &["env", "show", "demo", "--json"]);
        assert_eq!(stdout(&before_env), stdout(&after_env));
        let history = run_ocm(root.path(), &env, &["upgrade", "history", "demo", "--json"]);
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&history)).unwrap(),
            json!([])
        );
    }
    for line in fs::read_to_string(&trace).unwrap().lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        assert_ne!(
            event["event"], "child_start",
            "inspection started a Git child: {event}"
        );
    }
    // Once the replacement is committed, stale submodule metadata is harmless.
    git(&repo, &["commit", "-m", "replace submodule"]);
    assert_eq!(inspect(&root, &env)["source"]["workingTreeClean"], true);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn source_job_refusal(root: &TestDir, env: &BTreeMap<String, String>) -> Value {
    use std::time::{Duration, Instant};
    let accepted = run_ocm(
        root.path(),
        env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--if-binding",
            "launcher:source",
        ],
    );
    assert!(accepted.status.success(), "{}", stderr(&accepted));
    let accepted: Value = serde_json::from_str(&stdout(&accepted)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = run_ocm(
            root.path(),
            env,
            &[
                "upgrade",
                "job",
                "status",
                "demo",
                "--request-id",
                accepted["id"].as_str().unwrap(),
            ],
        );
        assert!(status.status.success(), "{}", stderr(&status));
        let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
        if status["state"] == "failed" {
            return status;
        }
        assert_eq!(status["state"], "running", "{status}");
        assert!(
            Instant::now() < deadline,
            "source observation job did not finish"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_report_survives_the_asynchronous_job_result() {
    use std::time::{Duration, Instant};
    let root = TestDir::new("source-job-result");
    let (env, mut state) = execution_fixture(&root, false);
    state["gate"] = json!(root.child("gate"));
    write_text(&root.child("native.json"), &state.to_string());
    for args in [
        vec!["launcher", "add", "other", "--command", "echo other"],
        vec!["env", "create", "other", "--launcher", "other"],
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let accepted = run_ocm(root.path(), &env, &["upgrade", "job", "start", "demo"]);
    assert!(accepted.status.success(), "{}", stderr(&accepted));
    let accepted: Value = serde_json::from_str(&stdout(&accepted)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.child("gate.started").exists() {
        assert!(Instant::now() < deadline, "native child did not start");
        std::thread::sleep(Duration::from_millis(25));
    }
    let mut policy = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(root.path())
        .env_clear()
        .envs(&env)
        .args(["service", "stop", "other", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        policy.try_wait().unwrap().is_none(),
        "service policy write bypassed source exclusion"
    );
    let read = run_ocm(root.path(), &env, &["env", "show", "other", "--json"]);
    assert!(read.status.success(), "{}", stderr(&read));
    // The native checkout may temporarily lack its entry/package during promotion.
    // Accepted-job transport remains readable independently of current feasibility.
    let entry = root.child("source/openclaw.mjs");
    let package = root.child("source/package.json");
    fs::rename(&entry, root.child("saved-entry")).unwrap();
    fs::rename(&package, root.child("saved-package")).unwrap();
    let capabilities = run_ocm(
        root.path(),
        &env,
        &["upgrade", "job", "capabilities", "demo"],
    );
    assert!(capabilities.status.success(), "{}", stderr(&capabilities));
    let capabilities: Value = serde_json::from_str(&stdout(&capabilities)).unwrap();
    assert!(
        capabilities["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("packaged-upgrade"))
    );
    let mut observed_running = false;
    loop {
        let status = run_ocm(
            root.path(),
            &env,
            &[
                "upgrade",
                "job",
                "status",
                "demo",
                "--request-id",
                accepted["id"].as_str().unwrap(),
            ],
        );
        assert!(status.status.success(), "{}", stderr(&status));
        let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
        if status["state"] == "succeeded" {
            assert!(observed_running);
            assert_eq!(status["result"]["outcome"], "source-updated");
            let current = run_ocm(
                root.path(),
                &env,
                &["upgrade", "demo", "--dry-run", "--json"],
            );
            assert!(current.status.success(), "{}", stderr(&current));
            let current: Value = serde_json::from_str(&stdout(&current)).unwrap();
            assert_eq!(status["result"]["source"], current["source"]);
            break;
        }
        assert_eq!(status["state"], "running", "{status}");
        if !observed_running {
            observed_running = true;
            fs::rename(root.child("saved-entry"), &entry).unwrap();
            fs::rename(root.child("saved-package"), &package).unwrap();
            write_text(&root.child("gate.release"), "release");
        }
        assert!(
            Instant::now() < deadline,
            "source observation job did not finish"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    while policy.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            policy.kill().unwrap();
            panic!("service policy remained blocked after native exit");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let policy = policy.wait_with_output().unwrap();
    assert!(policy.status.success(), "{}", stderr(&policy));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn execution_fixture(root: &TestDir, current: bool) -> (BTreeMap<String, String>, Value) {
    let (mut env, _) = fixture(root);
    let repo = root.child("source");
    write_text(
        &repo.join("openclaw.mjs"),
        r#"
import fs from 'node:fs';
import path from 'node:path';
import { setTimeout } from 'node:timers/promises';
const file = process.env.OCM_TEST_SOURCE_STATE;
const state = JSON.parse(fs.readFileSync(file));
const args = process.argv.slice(2);
fs.appendFileSync(file + '.calls', JSON.stringify({args, repair: process.env.OPENCLAW_SERVICE_REPAIR_POLICY,
  supervisor: process.env.OPENCLAW_SUPERVISOR_MODE, cache: process.env.NODE_DISABLE_COMPILE_CACHE}) + '\n');
if (args[0] === 'update' && args[1] === 'status') {
  console.log(JSON.stringify(state.status));
} else if (args[0] === 'status') {
  console.log(JSON.stringify(state.localStatus ?? state.status));
  process.exitCode = state.localExitCode ?? 0;
} else if (args[0] === 'update' && args.includes('--no-restart')) {
  if (state.gate) {
    fs.writeFileSync(state.gate + '.started', String(process.pid));
    const deadline = Date.now() + 15000;
    while (!fs.existsSync(state.gate + '.release') && Date.now() < deadline) await setTimeout(20);
    if (!fs.existsSync(state.gate + '.release')) throw new Error('fixture gate expired');
  }
  if (state.nextStatus) {
    fs.writeFileSync(file, JSON.stringify({...state, status: state.nextStatus}));
    fs.writeFileSync(path.join(state.status.update.root, 'dist/build-info.json'), JSON.stringify({
      commit: state.nextStatus.update.git.sha, version: '2026.9.3', buildId: 'new-source'
    }));
  }
  fs.writeFileSync(path.join(process.env.OPENCLAW_STATE_DIR, 'source-witness'), 'native-result');
  if (state.gate) fs.writeFileSync(state.gate + '.written', 'done');
  console.log(JSON.stringify(state.result));
  process.exitCode = state.exitCode ?? 0;
} else if (args[0] === '--version') {
  console.log('2026.9.3');
} else {
  throw new Error('unexpected native command: ' + args.join(' '));
}
"#,
    );
    git(&repo, &["add", "openclaw.mjs"]);
    git(&repo, &["commit", "-m", "native command fixture"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let built = if current {
        head.clone()
    } else {
        "b".repeat(40)
    };
    write_text(
        &repo.join("dist/build-info.json"),
        &json!({
            "version":"2026.9.3", "commit":built, "buildId":"old-source"
        })
        .to_string(),
    );
    let command = format!("node {}", path_string(&repo.join("openclaw.mjs")));
    for args in [
        vec![
            "launcher",
            "add",
            "built-source",
            "--command",
            command.as_str(),
        ],
        vec!["env", "set-launcher", "demo", "built-source"],
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "unsupported".into());
    env.insert(
        "OCM_TEST_SOURCE_STATE".into(),
        path_string(&root.child("native.json")),
    );
    let status = json!({
        "channel":{"value":"dev"},
        "update":{"root":repo,"installKind":"git","git":{
            "sha":head,"builtSha":built,"upstreamSha":head,"branch":"main","fetchOk":true,"dirty":false,
            "artifacts":{"ready":current,"version":"2026.9.3","buildId":"old-source"}
        }}
    });
    let mut next = status.clone();
    next["update"]["git"]["builtSha"] = json!(head);
    next["update"]["git"]["artifacts"] =
        json!({"ready":true,"version":"2026.9.3","buildId":"new-source"});
    let state = json!({"status":status,"nextStatus":next,"result":{
        "status":"ok","mode":"git","root":repo,
        "before":{"sha":head,"version":"2026.9.3","buildId":"old-source"},
        "after":{"sha":head,"version":"2026.9.3","buildId":"new-source"}
    }});
    write_text(&root.child("native.json"), &state.to_string());
    write_text(
        &root.child("ocm-home/envs/demo/.openclaw/source-witness"),
        "original",
    );
    (env, state)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_current_and_legacy_native_support_leave_state_untouched() {
    for supported in [true, false] {
        let root = TestDir::new("source-current");
        let (env, mut state) = execution_fixture(&root, true);
        if !supported {
            state["status"]["update"]["git"]
                .as_object_mut()
                .unwrap()
                .remove("artifacts");
            write_text(&root.child("native.json"), &state.to_string());
        }
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(
            value["outcome"],
            if supported {
                "up-to-date"
            } else {
                "local-command"
            }
        );
        assert!(value["snapshotId"].is_null());
        assert_eq!(
            fs::read_to_string(root.child("ocm-home/envs/demo/.openclaw/source-witness")).unwrap(),
            "original"
        );
        let calls = fs::read_to_string(root.child("native.json.calls")).unwrap();
        assert_eq!(calls.lines().count(), 1);
        let call: Value = serde_json::from_str(calls.trim()).unwrap();
        assert_eq!(call["args"], json!(["update", "status", "--json"]));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn stopped_source_upgrade_requires_a_confirmed_absent_managed_child() {
    for observation in ["exiting", "missing", "stopped"] {
        let root = TestDir::new("source-stopped-observation");
        let (mut env, _) = execution_fixture(&root, true);
        support::install_fake_service_manager(&root, &mut env);
        support::enable_fake_daemon_gateway_admission(&root, &mut env);
        let installed = run_ocm(root.path(), &env, &["service", "install", "demo", "--json"]);
        assert!(installed.status.success(), "{}", stderr(&installed));
        let runtime_path = ocm::store::supervisor_runtime_path(&env, root.path()).unwrap();
        if observation == "missing" {
            fs::remove_file(&runtime_path).unwrap();
        } else if observation == "exiting" {
            let mut runtime: Value =
                serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
            runtime["children"] = json!([ocm::supervisor::SupervisorRuntimeChild {
                env_name: "demo".to_string(),
                binding_kind: "launcher".to_string(),
                binding_name: "built-source".to_string(),
                pid: std::process::id(),
                launch_spec_sha256: None,
                restart_count: 0,
                child_port: 19445,
                stdout_path: path_string(&root.child("child.out")),
                stderr_path: path_string(&root.child("child.err")),
            }]);
            support::write_json_replacing_path(&runtime_path, &runtime);
        }
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        if observation == "stopped" {
            assert!(output.status.success(), "{}", stderr(&output));
            assert_eq!(
                serde_json::from_str::<Value>(&stdout(&output)).unwrap()["outcome"],
                "up-to-date"
            );
        } else {
            assert!(!output.status.success());
            assert!(
                stderr(&output).contains("cannot confirm the source Gateway is stopped"),
                "{}",
                stderr(&output)
            );
        }
        let snapshots = run_ocm(
            root.path(),
            &env,
            &["env", "snapshot", "list", "demo", "--json"],
        );
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&snapshots)).unwrap(),
            json!([])
        );
        let calls = fs::read_to_string(root.child("native.json.calls")).unwrap();
        assert!(!calls.contains("--no-restart"));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_commands_resolve_relative_node_from_launcher_directory() {
    let root = TestDir::new("source-relative-node");
    let (env, _) = execution_fixture(&root, true);
    let tools = root.child("tools");
    support::write_executable_script(&tools.join("bin/node"), "#!/bin/sh\nexec node \"$@\"\n");
    let command = format!(
        "./bin/node {}",
        path_string(&root.child("source/openclaw.mjs"))
    );
    for args in [
        vec![
            "launcher",
            "add",
            "relative-source",
            "--command",
            &command,
            "--cwd",
            tools.to_str().unwrap(),
        ],
        vec!["env", "set-launcher", "demo", "relative-source"],
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(result["outcome"], "up-to-date");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn release_source_noop_requires_the_native_configured_target_fact() {
    for scenario in [
        "stable",
        "beta",
        "missing",
        "older",
        "mismatched-channel",
        "extended-stable",
    ] {
        let root = TestDir::new("release-source-target");
        let (env, mut state) = execution_fixture(&root, true);
        let channel = if scenario == "beta" {
            "beta"
        } else if scenario == "extended-stable" {
            "extended-stable"
        } else {
            "stable"
        };
        state["status"]["channel"] = json!({"value":channel,"config":channel});
        state["status"]["update"]["git"]["fetchOk"] = json!(false);
        let head = state["status"]["update"]["git"]["sha"].clone();
        state["status"]["update"]["git"]["preferredTarget"] =
            json!({"channel":channel,"tag":"v2026.9.3","sha":head});
        match scenario {
            "missing" => {
                state["status"]["update"]["git"]
                    .as_object_mut()
                    .unwrap()
                    .remove("preferredTarget");
            }
            "older" => {
                state["status"]["update"]["git"]["preferredTarget"]["sha"] = json!("a".repeat(40));
            }
            "mismatched-channel" => {
                state["status"]["channel"]["config"] = json!("beta");
            }
            _ => {}
        }
        write_text(&root.child("native.json"), &state.to_string());
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        assert!(output.status.success(), "{scenario}: {}", stderr(&output));
        let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(
            result["outcome"],
            if matches!(scenario, "stable" | "beta") {
                "up-to-date"
            } else {
                "source-updated"
            },
            "{scenario}"
        );
        if matches!(scenario, "stable" | "beta") {
            assert!(result["snapshotId"].is_null());
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_unfinished_updates_refuse_before_checkpoint_or_service_changes() {
    for field in [
        "activeRun",
        "staleRun",
        "abandonedRun",
        "runStatusError",
        "runReconciliationError",
        "lastRun",
    ] {
        let root = TestDir::new("source-native-run");
        let (env, mut state) = execution_fixture(&root, true);
        state["status"][field] = json!({"status":"failed","runId":"native-fixture"});
        write_text(&root.child("native.json"), &state.to_string());
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        assert_eq!(
            output.status.success(),
            field == "lastRun",
            "{field}: {}",
            stderr(&output)
        );
        if field != "lastRun" {
            assert!(stderr(&output).contains("native update activity or recovery is unresolved"));
        }
        let snapshots = run_ocm(
            root.path(),
            &env,
            &["env", "snapshot", "list", "demo", "--json"],
        );
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&snapshots)).unwrap(),
            json!([])
        );
        assert_eq!(
            fs::read_to_string(root.child("native.json.calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_batch_update_preserves_binding_and_records_non_rollbackable_success() {
    let root = TestDir::new("source-update");
    let (env, _) = execution_fixture(&root, false);
    let output = run_ocm(root.path(), &env, &["upgrade", "--all", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let batch: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(batch["changed"], 1);
    assert_eq!(batch["failed"], 0);
    let value = &batch["results"][0];
    assert_eq!(value["outcome"], "source-updated");
    assert_eq!(value["bindingKind"], "launcher");
    assert_eq!(value["bindingName"], "built-source");
    assert!(value["serviceAction"].is_null());
    assert!(value["snapshotId"].is_string());
    let shown = run_ocm(root.path(), &env, &["env", "show", "demo", "--json"]);
    let shown: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown["defaultLauncher"], "built-source");
    assert!(shown["defaultRuntime"].is_null());
    let calls: Vec<Value> = fs::read_to_string(root.child("native.json.calls"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(calls.iter().all(|call| call["repair"] == "external"
        && call["cache"] == "1"
        && call["supervisor"].is_null()));
    assert_eq!(
        calls.iter().map(|call| &call["args"]).collect::<Vec<_>>(),
        vec![
            &json!(["update", "status", "--json"]),
            &json!(["update", "--no-restart", "--json"]),
            &json!(["status", "--json"]),
        ]
    );
    let history = run_ocm(root.path(), &env, &["upgrade", "history", "demo", "--json"]);
    let history: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history[0]["outcome"], "source-updated");
    let rollback = run_ocm(
        root.path(),
        &env,
        &[
            "upgrade",
            "rollback",
            "demo",
            "--transaction",
            history[0]["id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert!(!rollback.status.success());
    assert!(
        stderr(&rollback).contains("source-updated"),
        "{}",
        stderr(&rollback)
    );
    assert_eq!(
        fs::read_to_string(root.child("ocm-home/envs/demo/.openclaw/source-witness")).unwrap(),
        "native-result"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_failure_does_not_restore_old_state_over_unverified_source() {
    let root = TestDir::new("source-uncertain-recovery");
    let (env, mut state) = execution_fixture(&root, false);
    state["exitCode"] = json!(1);
    state["result"]["status"] = json!("error");
    state["result"]["reason"] = json!("fixture native failure");
    state["result"]["pluginData"] = json!({"token":"private-fixture-value"});
    state["result"]["recovery"] =
        json!({"serviceRestartSafe":false,"reason":"state-migration-started"});
    write_text(&root.child("native.json"), &state.to_string());
    let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["outcome"], "failed");
    assert!(
        value["note"]
            .as_str()
            .unwrap()
            .starts_with("recovery is unresolved")
    );
    assert!(
        value["note"]
            .as_str()
            .unwrap()
            .contains("fixture native failure")
    );
    assert!(!stdout(&output).contains("private-fixture-value"));
    assert!(value["snapshotId"].is_string());
    assert_eq!(
        fs::read_to_string(root.child("ocm-home/envs/demo/.openclaw/source-witness")).unwrap(),
        "native-result"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_verification_falls_back_only_for_inconclusive_local_status() {
    for scenario in [
        "cold",
        "legacy",
        "scan-failure",
        "unknown-probe",
        "failed-probe",
        "canonical-mismatch",
        "unready",
        "wrong-root",
        "wrong-kind",
        "stale-build",
        "wrong-build-id",
        "refusal",
        "interrupted",
    ] {
        let root = TestDir::new("source-local-verification");
        let (env, mut state) = execution_fixture(&root, false);
        state["localStatus"] = state["nextStatus"].clone();
        match scenario {
            "cold" => {
                state["localStatus"] = json!({"update":{"root":null,"installKind":"unknown"}})
            }
            "legacy" => {
                state["localStatus"]["update"]["git"]
                    .as_object_mut()
                    .unwrap()
                    .remove("artifacts");
            }
            "scan-failure" => {
                state["localStatus"] = json!({"ok":false,"error":{"type":"cli_error","message":"optional session scan unavailable"}});
                state["localExitCode"] = json!(1);
            }
            "unknown-probe" | "failed-probe" => {
                state["localStatus"]["update"]["error"] = json!({"status":
                    if scenario == "unknown-probe" { "unknown" } else { "failed" }
                });
            }
            "canonical-mismatch" => {
                state["localStatus"] = json!({"update":{"root":null,"installKind":"unknown"}});
                state["nextStatus"]["update"]["root"] = json!(root.path());
            }
            "unready" => state["localStatus"]["update"]["git"]["artifacts"]["ready"] = json!(false),
            "wrong-root" => state["localStatus"]["update"]["root"] = json!(root.path()),
            "wrong-kind" => state["localStatus"]["update"]["installKind"] = json!("npm"),
            "stale-build" => {
                state["localStatus"]["update"]["git"]["builtSha"] = json!("a".repeat(40))
            }
            "wrong-build-id" => {
                state["localStatus"]["update"]["git"]["artifacts"]["buildId"] =
                    json!("unexpected-build")
            }
            "refusal" => {
                state["localStatus"] =
                    json!({"ok":false,"error":{"type":"cli_error","code":"schema-refusal"}});
                state["localExitCode"] = json!(1);
            }
            "interrupted" => state["localExitCode"] = json!(130),
            _ => unreachable!(),
        }
        write_text(&root.child("native.json"), &state.to_string());
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        let fallback = matches!(
            scenario,
            "cold" | "legacy" | "scan-failure" | "unknown-probe" | "canonical-mismatch"
        );
        assert_eq!(
            output.status.success(),
            fallback && scenario != "canonical-mismatch",
            "{scenario}: {}",
            stdout(&output)
        );
        let calls: Vec<Value> = fs::read_to_string(root.child("native.json.calls"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["args"].clone())
            .collect();
        let mut expected = vec![
            json!(["update", "status", "--json"]),
            json!(["update", "--no-restart", "--json"]),
            json!(["status", "--json"]),
        ];
        if fallback {
            expected.push(json!(["update", "status", "--json"]));
        }
        assert_eq!(calls, expected, "{scenario}");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_recovery_requires_fresh_local_identity_or_canonical_fallback() {
    for scenario in [
        "retained",
        "cold",
        "wrong-build-id",
        "wrong-commit",
        "unready",
    ] {
        let root = TestDir::new("source-local-recovery");
        let (env, mut state) = execution_fixture(&root, true);
        state["status"]["update"]["git"]["fetchOk"] = json!(false);
        state.as_object_mut().unwrap().remove("nextStatus");
        state["exitCode"] = json!(1);
        state["result"]["status"] = json!("error");
        state["result"]["reason"] = json!("native refusal");
        state["result"]["recovery"] =
            json!({"serviceRestartSafe":true,"version":"2026.9.3","buildId":"old-source"});
        state["localStatus"] = state["status"].clone();
        match scenario {
            "cold" => {
                state["localStatus"] = json!({"update":{"root":null,"installKind":"unknown"}})
            }
            "wrong-build-id" => {
                state["localStatus"]["update"]["git"]["artifacts"]["buildId"] =
                    json!("unexpected-build")
            }
            "wrong-commit" => {
                state["localStatus"]["update"]["git"]["sha"] = json!("a".repeat(40));
                state["localStatus"]["update"]["git"]["builtSha"] = json!("a".repeat(40));
            }
            "unready" => state["localStatus"]["update"]["git"]["artifacts"]["ready"] = json!(false),
            _ => {}
        }
        write_text(&root.child("native.json"), &state.to_string());
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        assert!(!output.status.success());
        let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
        let recovered = matches!(scenario, "retained" | "cold");
        assert_eq!(
            result["note"]
                .as_str()
                .unwrap()
                .starts_with("native recovery verified"),
            recovered,
            "{scenario}: {result}"
        );
        let calls: Vec<Value> = fs::read_to_string(root.child("native.json.calls"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["args"].clone())
            .collect();
        assert_eq!(calls[2], json!(["status", "--json"]));
        assert_eq!(
            calls.len(),
            if scenario == "cold" { 4 } else { 3 },
            "{scenario}"
        );
        if scenario == "cold" {
            assert_eq!(calls[3], json!(["update", "status", "--json"]));
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_refusal_is_not_source_success_even_with_current_artifacts() {
    let root = TestDir::new("source-native-refusal");
    let (env, mut state) = execution_fixture(&root, true);
    state["status"]["update"]["git"]["fetchOk"] = json!(false);
    state.as_object_mut().unwrap().remove("nextStatus");
    state["result"]["status"] = json!("skipped");
    state["result"]["reason"] = json!("no-upstream");
    state["result"]["after"] = state["result"]["before"].clone();
    state["result"]["recovery"] =
        json!({"serviceRestartSafe":true,"version":"2026.9.3","buildId":"old-source"});
    write_text(&root.child("native.json"), &state.to_string());
    let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
    assert!(!output.status.success());
    let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(result["outcome"], "failed");
    assert!(result["note"].as_str().unwrap().contains("no-upstream"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_admission_refuses_dirty_shared_and_unreadable_bindings_before_execution() {
    for scenario in [
        "dirty",
        "shared",
        "shared-quoted",
        "unreadable",
        "assume-unchanged",
        "skip-worktree",
        "staged-gitlink",
    ] {
        let root = TestDir::new("source-admission");
        let (env, _) = execution_fixture(&root, false);
        if scenario == "dirty" {
            write_text(&root.child("source/operator-notes.txt"), "preserve");
        } else if scenario == "staged-gitlink" {
            let repo = root.child("source");
            let head = git(&repo, &["rev-parse", "HEAD"]);
            git(
                &repo,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{head},nested"),
                ],
            );
            git(&repo, &["commit", "-m", "record gitlink"]);
            git(&repo, &["update-index", "--force-remove", "nested"]);
        } else if matches!(scenario, "assume-unchanged" | "skip-worktree") {
            git(
                &root.child("source"),
                &["update-index", &format!("--{scenario}"), "openclaw.mjs"],
            );
            let entry = root.child("source/openclaw.mjs");
            let mut contents = fs::read_to_string(&entry).unwrap();
            contents.push_str("\n// operator change hidden from ordinary Git status\n");
            write_text(&entry, &contents);
            assert!(git(&root.child("source"), &["status", "--porcelain"]).is_empty());
        } else {
            let peer_launcher = if scenario == "shared-quoted" {
                let alias = root.child("Source #1 & Data");
                std::os::unix::fs::symlink(root.child("source"), &alias).unwrap();
                let command = format!("node \"{}\"", alias.join("openclaw.mjs").display());
                let added = run_ocm(
                    root.path(),
                    &env,
                    &["launcher", "add", "quoted-peer", "--command", &command],
                );
                assert!(added.status.success(), "{}", stderr(&added));
                "quoted-peer"
            } else {
                "built-source"
            };
            let created = run_ocm(
                root.path(),
                &env,
                &["env", "create", "other", "--launcher", peer_launcher],
            );
            assert!(created.status.success(), "{}", stderr(&created));
            if scenario == "unreadable" {
                let added = run_ocm(
                    root.path(),
                    &env,
                    &["launcher", "add", "missing", "--command", "echo other"],
                );
                assert!(added.status.success(), "{}", stderr(&added));
                let bound = run_ocm(
                    root.path(),
                    &env,
                    &["env", "set-launcher", "other", "missing"],
                );
                assert!(bound.status.success(), "{}", stderr(&bound));
                fs::remove_file(
                    ocm::store::launcher_meta_path("missing", &env, root.path()).unwrap(),
                )
                .unwrap();
            }
        }
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        assert!(!output.status.success(), "{scenario}: {}", stdout(&output));
        assert!(
            !root.child("native.json.calls").exists(),
            "{scenario}: native command executed"
        );
        let snapshots = run_ocm(
            root.path(),
            &env,
            &["env", "snapshot", "list", "demo", "--json"],
        );
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&snapshots)).unwrap(),
            json!([])
        );
        let capabilities = run_ocm(
            root.path(),
            &env,
            &["upgrade", "job", "capabilities", "demo"],
        );
        assert!(
            capabilities.status.success(),
            "{scenario}: {}",
            stderr(&capabilities)
        );
        let capabilities: Value = serde_json::from_str(&stdout(&capabilities)).unwrap();
        assert!(
            capabilities["operations"]
                .as_array()
                .unwrap()
                .contains(&json!("packaged-upgrade"))
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn source_child_retains_registry_exclusion_after_parent_loss() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::thread::sleep;
    use std::time::{Duration, Instant};
    struct Group(u32);
    impl Drop for Group {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.0 as i32), libc::SIGKILL);
            }
        }
    }
    let root = TestDir::new("source-registry-custody");
    let (env, mut state) = execution_fixture(&root, false);
    state["gate"] = json!(root.child("gate"));
    write_text(&root.child("native.json"), &state.to_string());
    for args in [
        vec!["launcher", "add", "other", "--command", "echo other"],
        vec!["env", "create", "other", "--launcher", "other"],
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let mut update = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(root.path())
        .env_clear()
        .envs(&env)
        .args(["upgrade", "demo", "--json"])
        .stdin(Stdio::null())
        .stdout(fs::File::create(root.child("update.out")).unwrap())
        .stderr(fs::File::create(root.child("update.err")).unwrap())
        .process_group(0)
        .spawn()
        .unwrap();
    let _group = Group(update.id());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.child("gate.started").exists() {
        assert!(
            Instant::now() < deadline,
            "native child did not start: {:?}",
            fs::read_to_string(root.child("update.err"))
        );
        sleep(Duration::from_millis(25));
    }
    update.kill().unwrap();
    update.wait().unwrap();
    let mut binding = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(root.path())
        .env_clear()
        .envs(&env)
        .args(["env", "set-launcher", "other", "built-source"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(200));
    assert!(
        binding.try_wait().unwrap().is_none(),
        "new source consumer was published while native could mutate it"
    );
    let read = run_ocm(root.path(), &env, &["env", "show", "other", "--json"]);
    assert!(read.status.success(), "{}", stderr(&read));
    write_text(&root.child("gate.release"), "release");
    let deadline = Instant::now() + Duration::from_secs(10);
    while binding.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            binding.kill().unwrap();
            panic!("binding remained blocked after native exit");
        }
        sleep(Duration::from_millis(25));
    }
    let bound = binding.wait_with_output().unwrap();
    assert!(bound.status.success(), "{}", stderr(&bound));
    assert!(root.child("gate.written").exists());
}
