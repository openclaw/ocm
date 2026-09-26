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
    let raw = run_ocm(root.path(), &env, &["upgrade", "demo", "--raw"]);
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert!(stdout(&raw).contains("buildMatchesHead=false"));
    let batch = run_ocm(root.path(), &env, &["upgrade", "--all", "--json"]);
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
        let output = run_ocm(root.path(), &env, &["upgrade", "demo", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(result.get("source").is_some(), recognized);
        assert_eq!(result["outcome"], "local-command");
    }
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

#[cfg(unix)]
#[test]
fn source_report_survives_the_asynchronous_job_result() {
    use std::time::{Duration, Instant};
    let root = TestDir::new("source-job-result");
    let (env, _) = fixture(&root);
    let expected = inspect(&root, &env);
    let accepted = run_ocm(root.path(), &env, &["upgrade", "job", "start", "demo"]);
    assert!(accepted.status.success(), "{}", stderr(&accepted));
    let accepted: Value = serde_json::from_str(&stdout(&accepted)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
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
            assert_eq!(status["result"], expected);
            break;
        }
        assert_eq!(status["state"], "running", "{status}");
        assert!(
            Instant::now() < deadline,
            "source observation job did not finish"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
