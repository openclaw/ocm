#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::{
    TestDir, TestHttpServer, ocm_env, path_string, run_ocm, stderr, stdout, write_executable_script,
};

fn command(root: &TestDir, env: &BTreeMap<String, String>, args: &[&str]) -> Value {
    let output = run_ocm(root.path(), env, args);
    assert!(output.status.success(), "{}", stderr(&output));
    serde_json::from_str(&stdout(&output)).unwrap()
}

fn setup(root: &TestDir) -> BTreeMap<String, String> {
    let mut env = ocm_env(root);
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "unsupported".into());
    for (name, version) in [("old", "2026.3.24"), ("new", "2026.3.25")] {
        let binary = root.child(name);
        write_executable_script(
            &binary,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{calls}'
case "$1" in
  --version) echo '{version}';;
  doctor) echo '{{"ok":true,"checksRun":1,"checksSkipped":0,"findings":[]}}';;
  update)
    : > '{started}'
    attempts=0
    while [ ! -f '{release}' ]; do
      attempts=$((attempts + 1))
      [ "$attempts" -lt 400 ] || exit 9
      sleep 0.05
    done
    echo '{{"status":"ok","mode":"finalize","postUpdate":{{"doctor":{{"status":"ok"}},"plugins":{{"status":"ok"}}}}}}'
    ;;
  *) echo '{{}}';;
esac
"#,
                calls = root.child(format!("{name}-calls")).display(),
                started = root.child("started").display(),
                release = root.child("release").display()
            ),
        );
        let output = run_ocm(
            root.path(),
            &env,
            &["runtime", "add", name, "--path", &path_string(&binary)],
        );
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let output = run_ocm(
        root.path(),
        &env,
        &["env", "create", "demo", "--runtime", "old"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    env
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "worker never reached {}",
            path.display()
        );
        sleep(Duration::from_millis(25));
    }
}

fn wait_for_result(root: &TestDir, env: &BTreeMap<String, String>, id: &str) -> Value {
    wait_for_environment_result(root, env, "demo", id)
}

fn wait_for_environment_result(
    root: &TestDir,
    env: &BTreeMap<String, String>,
    name: &str,
    id: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = command(
            root,
            env,
            &["upgrade", "job", "status", name, "--request-id", id],
        );
        if matches!(
            status["state"].as_str(),
            Some("succeeded" | "failed" | "interrupted")
        ) {
            return status;
        }
        assert!(Instant::now() < deadline, "job did not finish: {status}");
        sleep(Duration::from_millis(25));
    }
}

#[test]
fn case_distinct_environments_select_their_own_jobs() {
    use std::os::unix::fs::MetadataExt;

    let root = TestDir::new("upgrade-case-distinct-jobs");
    let env = setup(&root);
    command(
        &root,
        &env,
        &[
            "env",
            "create",
            "Demo",
            "--runtime",
            "old",
            "--root",
            &path_string(&root.child("upper")),
            "--json",
        ],
    );
    fs::write(root.child("release"), "").unwrap();
    let job_dir = |name| {
        ocm::store::upgrade_history_env_dir(name, &env, root.path())
            .unwrap()
            .join("jobs")
    };
    let mut latest = BTreeMap::new();
    let mut historical = Vec::new();
    // Reverse lexical order: latest is submission time, not the request ID.
    for (name, id, target, binding) in [
        ("Demo", "z-upper-first", "new", "runtime:old"),
        ("demo", "z-lower-first", "new", "runtime:old"),
        ("Demo", "a-upper-next", "new", "runtime:new"),
        ("demo", "a-lower-next", "new", "runtime:new"),
    ] {
        command(
            &root,
            &env,
            &[
                "upgrade",
                "job",
                "start",
                name,
                "--runtime",
                target,
                "--request-id",
                id,
                "--if-binding",
                binding,
            ],
        );
        let result = wait_for_environment_result(&root, &env, name, id);
        assert_eq!(result["state"], "succeeded", "{result}");
        latest.insert(name, result.clone());
        historical.push((name, id, target, binding, result));
        for owner in ["Demo", "demo"] {
            let status = command(&root, &env, &["upgrade", "job", "status", owner]);
            assert_eq!(status, latest.get(owner).cloned().unwrap_or(Value::Null));
        }
        for (owner, request, target, binding, result) in &historical {
            assert_eq!(
                command(
                    &root,
                    &env,
                    &["upgrade", "job", "status", owner, "--request-id", request]
                ),
                *result,
            );
            assert_eq!(
                command(
                    &root,
                    &env,
                    &[
                        "upgrade",
                        "job",
                        "start",
                        owner,
                        "--runtime",
                        target,
                        "--request-id",
                        request,
                        "--if-binding",
                        binding
                    ]
                ),
                *result,
            );
        }
    }
    let upper = fs::metadata(job_dir("Demo")).unwrap();
    let lower = fs::metadata(job_dir("demo")).unwrap();
    let shared = (upper.dev(), upper.ino()) == (lower.dev(), lower.ino());
    eprintln!("case-distinct job directories share native identity: {shared}");
    let peer_path = job_dir("demo").join("a-lower-next.json");
    let peer_before = fs::read(&peer_path).unwrap();
    for args in [
        vec![
            "upgrade",
            "job",
            "status",
            "Demo",
            "--request-id",
            "a-lower-next",
        ],
        vec![
            "upgrade",
            "job",
            "start",
            "Demo",
            "--runtime",
            "old",
            "--request-id",
            "a-upper-next",
            "--if-binding",
            "runtime:new",
        ],
        vec![
            "upgrade",
            "job",
            "start",
            "Demo",
            "--runtime",
            "new",
            "--request-id",
            "a-upper-next",
        ],
    ] {
        let output = run_ocm(root.path(), &env, &args);
        assert!(!output.status.success(), "{args:?}");
    }
    if shared {
        let collision = run_ocm(
            root.path(),
            &env,
            &[
                "upgrade",
                "job",
                "start",
                "Demo",
                "--runtime",
                "new",
                "--request-id",
                "a-lower-next",
                "--if-binding",
                "runtime:new",
            ],
        );
        assert!(!collision.status.success());
        assert!(stderr(&collision).contains("mismatched upgrade job record"));

        // Inventory must validate records even when another owner is filtered out.
        // The pointer is the lower owner's, so querying Demo takes the fallback.
        let corrupt_path = job_dir("demo").join("z-lower-first.json");
        let original = fs::read(&corrupt_path).unwrap();
        for (field, invalid) in [
            ("schema", serde_json::json!(1)),
            ("id", serde_json::json!("different-id")),
            ("envName", serde_json::json!("unrelated")),
        ] {
            let mut record: Value = serde_json::from_slice(&original).unwrap();
            if field == "schema" {
                record[field] = invalid;
            } else {
                record["job"][field] = invalid;
            }
            fs::write(&corrupt_path, serde_json::to_vec(&record).unwrap()).unwrap();
            for args in [
                vec!["upgrade", "job", "status", "Demo"],
                vec![
                    "upgrade",
                    "job",
                    "start",
                    "Demo",
                    "--runtime",
                    "new",
                    "--request-id",
                    "must-not-start",
                ],
            ] {
                let output = run_ocm(root.path(), &env, &args);
                assert!(!output.status.success(), "{field}: {args:?}");
                assert!(
                    stderr(&output).contains("mismatched upgrade job record"),
                    "{}",
                    stderr(&output)
                );
            }
            assert!(!job_dir("Demo").join("must-not-start.json").exists());
            fs::write(&corrupt_path, &original).unwrap();
        }
    }
    assert_eq!(fs::read(&peer_path).unwrap(), peer_before);
    assert_eq!(
        command(&root, &env, &["upgrade", "job", "status", "Demo"]),
        latest["Demo"],
    );
}

#[test]
fn shared_job_status_does_not_observe_or_mutate_an_active_peer() {
    use std::os::unix::fs::MetadataExt;

    let root = TestDir::new("upgrade-case-distinct-active");
    let env = setup(&root);
    command(
        &root,
        &env,
        &[
            "env",
            "create",
            "Demo",
            "--runtime",
            "old",
            "--root",
            &path_string(&root.child("upper")),
            "--json",
        ],
    );
    let accepted = command(
        &root,
        &env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            "active-peer",
        ],
    );
    wait_for(&root.child("started"));
    let directory = ocm::store::upgrade_history_env_dir("demo", &env, root.path())
        .unwrap()
        .join("jobs");
    let peer = directory.join("active-peer.json");
    let before = fs::read(&peer).unwrap();
    assert!(command(&root, &env, &["upgrade", "job", "status", "Demo"]).is_null());
    let upper = ocm::store::upgrade_history_env_dir("Demo", &env, root.path())
        .unwrap()
        .join("jobs");
    if let Ok(meta) = fs::metadata(&upper)
        && (meta.dev(), meta.ino()) == {
            let meta = fs::metadata(&directory).unwrap();
            (meta.dev(), meta.ino())
        }
    {
        let refused = run_ocm(
            root.path(),
            &env,
            &[
                "upgrade",
                "job",
                "start",
                "Demo",
                "--runtime",
                "new",
                "--request-id",
                "blocked-peer",
            ],
        );
        assert!(!refused.status.success());
        assert!(
            stderr(&refused).contains("upgrade job is active"),
            "{}",
            stderr(&refused)
        );
        assert!(!upper.join("blocked-peer.json").exists());
    }
    assert_eq!(fs::read(&peer).unwrap(), before);
    fs::write(root.child("release"), "").unwrap();
    assert_eq!(
        wait_for_result(&root, &env, accepted["id"].as_str().unwrap())["state"],
        "succeeded"
    );
}

#[test]
fn detached_upgrade_preserves_exclusion_and_each_requests_result() {
    let root = TestDir::new("upgrade-job-results");
    let env = setup(&root);
    let capabilities = command(&root, &env, &["upgrade", "job", "capabilities", "demo"]);
    assert_eq!(capabilities["protocol"], "ocm.upgrade-job");
    assert_eq!(capabilities["protocolVersion"], 1);
    assert_eq!(capabilities["supported"], true);
    assert_eq!(capabilities["envName"], "demo");
    assert_eq!(capabilities["bindingKind"], "runtime");
    assert_eq!(capabilities["bindingName"], "old");
    let binding = format!(
        "{}:{}",
        capabilities["bindingKind"].as_str().unwrap(),
        capabilities["bindingName"].as_str().unwrap()
    );
    assert_eq!(
        capabilities["envRoot"],
        path_string(&root.child("ocm-home/envs/demo"))
    );
    assert!(command(&root, &env, &["upgrade", "job", "status", "demo"]).is_null());

    let accepted = command(
        &root,
        &env,
        &[
            "upgrade",
            "job",
            "start",
            " demo ",
            "--runtime",
            "new",
            "--request-id",
            "admission",
            "--if-binding",
            &binding,
            "--json",
        ],
    );
    let id = accepted["id"].as_str().unwrap();
    assert_eq!(accepted["state"], "running");
    assert!(accepted["result"].is_null());
    assert_eq!(accepted["envName"], "demo");
    assert_eq!(accepted["id"], "admission");
    let record: Value = serde_json::from_slice(
        &fs::read(
            ocm::store::upgrade_history_env_dir("demo", &env, root.path())
                .unwrap()
                .join("jobs/admission.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        record["schema"], 2,
        "older workers must reject a guarded record"
    );
    let resumed = command(
        &root,
        &env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            id,
            "--if-binding",
            &binding,
        ],
    );
    assert_eq!(resumed["id"], accepted["id"]);

    wait_for(&root.child("started"));
    let duplicate = run_ocm(
        root.path(),
        &env,
        &["upgrade", "job", "start", "demo", "--runtime", "old"],
    );
    assert!(!duplicate.status.success());
    assert!(
        stderr(&duplicate).contains("upgrade job is active"),
        "{}",
        stderr(&duplicate)
    );
    fs::write(root.child("release"), "").unwrap();
    let result = wait_for_result(&root, &env, id);
    assert_eq!(result["state"], "succeeded", "{result}");
    assert_eq!(result["result"]["outcome"], "switched");
    assert_eq!(result["result"]["runtimeReleaseVersion"], "2026.3.25");
    assert!(result["revision"].as_u64() > accepted["revision"].as_u64());
    let history = command(&root, &env, &["upgrade", "history", "demo", "--json"]);
    assert_eq!(history.as_array().unwrap().len(), 1);

    let stale = run_ocm(
        root.path(),
        &env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            "stale",
            "--if-binding",
            &binding,
        ],
    );
    assert!(!stale.status.success());
    assert!(stderr(&stale).contains("environment binding changed"));
    assert_eq!(
        command(&root, &env, &["upgrade", "job", "status", "demo"]),
        result
    );
    assert_eq!(
        command(&root, &env, &["upgrade", "history", "demo", "--json"]),
        history
    );

    let second = command(
        &root,
        &env,
        &["upgrade", "job", "start", "demo", "--runtime", "missing"],
    );
    assert_ne!(second["id"], accepted["id"]);
    let failed = wait_for_result(&root, &env, second["id"].as_str().unwrap());
    assert_eq!(failed["state"], "failed");
    assert!(failed["error"].as_str().unwrap().contains("missing"));
    assert_eq!(
        command(&root, &env, &["upgrade", "job", "status", "demo"]),
        failed
    );
    assert_eq!(
        command(
            &root,
            &env,
            &["upgrade", "job", "status", "demo", "--request-id", id]
        ),
        result
    );
    let shown = command(&root, &env, &["env", "show", "demo", "--json"]);
    assert_eq!(shown["defaultRuntime"], "new");
    let replay_args = [
        "upgrade",
        "job",
        "start",
        "demo",
        "--runtime",
        "new",
        "--request-id",
        id,
        "--if-binding",
        &binding,
    ];
    assert_eq!(command(&root, &env, &replay_args), result);
    let mut changed_condition = replay_args;
    changed_condition[9] = "runtime:new";
    let changed = run_ocm(root.path(), &env, &changed_condition);
    assert!(!changed.status.success());
    assert!(
        stderr(&changed).contains("different binding condition"),
        "{}",
        stderr(&changed)
    );
    let removed = run_ocm(root.path(), &env, &["env", "destroy", "demo", "--yes"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    let recreated = run_ocm(
        root.path(),
        &env,
        &["env", "create", "demo", "--runtime", "old"],
    );
    assert!(recreated.status.success(), "{}", stderr(&recreated));
    let before = ["old", "new"]
        .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default());
    let replay = run_ocm(
        root.path(),
        &env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            id,
            "--if-binding",
            &binding,
        ],
    );
    assert!(!replay.status.success());
    assert!(
        stderr(&replay).contains("different environment instance"),
        "{}",
        stderr(&replay)
    );
    assert!(command(&root, &env, &["upgrade", "job", "status", "demo"]).is_null());
    assert_eq!(
        command(
            &root,
            &env,
            &["upgrade", "job", "status", "demo", "--request-id", id]
        ),
        result
    );
    assert_eq!(
        command(&root, &env, &["env", "show", "demo", "--json"])["defaultRuntime"],
        "old"
    );
    assert_eq!(
        before,
        ["old", "new"]
            .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default())
    );
}

#[test]
fn fresh_job_after_interruption_waits_for_surviving_native_writer() {
    use fs2::FileExt;
    let root = TestDir::new("upgrade-job-interrupted");
    let env = setup(&root);
    let accepted = command(
        &root,
        &env,
        &["upgrade", "job", "start", "demo", "--runtime", "new"],
    );
    let id = accepted["id"].as_str().unwrap();
    wait_for(&root.child("started"));
    let path = ocm::store::upgrade_history_env_dir("demo", &env, root.path())
        .unwrap()
        .join("jobs")
        .join(format!("{id}.json"));
    let record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let pid = i32::try_from(record["worker"]["pid"].as_u64().unwrap()).unwrap();
    // SAFETY: kill only this fixture's OCM worker, leaving its native writer alive.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    let interrupted = wait_for_result(&root, &env, id);
    assert_eq!(interrupted["state"], "interrupted");
    assert!(interrupted["result"].is_null());
    assert!(
        interrupted["error"]
            .as_str()
            .unwrap()
            .contains("recovery is unresolved")
    );
    let operation = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.child("ocm-home/locks/environments/demo.lock"))
        .unwrap();
    assert_eq!(
        operation.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "surviving native writer lost environment exclusion"
    );
    let calls = ["old", "new"]
        .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default());
    let replay = command(
        &root,
        &env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            id,
        ],
    );
    assert_eq!(replay, interrupted);
    let fresh = command(
        &root,
        &env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            "fresh-request",
        ],
    );
    assert_ne!(fresh["id"], accepted["id"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let waiting = command(
            &root,
            &env,
            &[
                "upgrade",
                "job",
                "status",
                "demo",
                "--request-id",
                "fresh-request",
            ],
        );
        assert_eq!(waiting["state"], "running", "{waiting}");
        if waiting["progress"] == "Upgrading environment" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fresh worker never entered the upgrade owner"
        );
        sleep(Duration::from_millis(25));
    }
    assert_eq!(
        operation.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        calls,
        ["old", "new"]
            .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default()),
        "fresh job performed runtime I/O while the previous writer retained the lock"
    );
    fs::write(root.child("release"), "").unwrap();
    let result = wait_for_result(&root, &env, "fresh-request");
    assert_eq!(result["state"], "succeeded", "{result}");
    assert_eq!(
        command(
            &root,
            &env,
            &["upgrade", "job", "status", "demo", "--request-id", id]
        ),
        interrupted
    );
}

#[test]
fn failed_response_does_not_admit_an_upgrade() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};

    let root = TestDir::new("upgrade-job-output-failure");
    let env = setup(&root);
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let writer: OwnedFd = writer.into();
    let output = Command::new(support::ocm_test_binary_path())
        .args([
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            "closed-reader",
        ])
        .env_clear()
        .envs(&env)
        .current_dir(root.path())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result = wait_for_result(&root, &env, "closed-reader");
    assert_eq!(result["state"], "failed", "{result}");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("no upgrade was started")
    );
    assert!(!root.child("started").exists());
    assert_eq!(
        command(&root, &env, &["env", "show", "demo", "--json"])["defaultRuntime"],
        "old"
    );
    assert!(
        command(&root, &env, &["upgrade", "history", "demo", "--json"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn admitted_job_cannot_upgrade_a_replacement_environment() {
    use fs2::FileExt;
    let root = TestDir::new("upgrade-job-replacement");
    let env = setup(&root);
    let locks = root.child("ocm-home/locks/upgrades");
    fs::create_dir_all(&locks).unwrap();
    let transaction = fs::File::create(locks.join("demo.lock")).unwrap();
    transaction.lock_exclusive().unwrap();
    let accepted = command(
        &root,
        &env,
        &["upgrade", "job", "start", "demo", "--runtime", "new"],
    );
    let removed = run_ocm(root.path(), &env, &["env", "destroy", "demo", "--yes"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    let recreated = run_ocm(
        root.path(),
        &env,
        &["env", "create", "demo", "--runtime", "old"],
    );
    assert!(recreated.status.success(), "{}", stderr(&recreated));
    let original_calls = ["old", "new"]
        .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default());
    let prior = run_ocm(root.path(), &env, &["upgrade", "job", "status", "demo"]);
    assert!(!prior.status.success());
    assert!(stderr(&prior).contains("prior environment instance still has active upgrade job"));
    let fresh = run_ocm(
        root.path(),
        &env,
        &[
            "upgrade",
            "job",
            "start",
            "demo",
            "--runtime",
            "new",
            "--request-id",
            "replacement-job",
        ],
    );
    assert!(!fresh.status.success());
    assert!(stderr(&fresh).contains("upgrade job is active"));
    transaction.unlock().unwrap();
    let result = wait_for_result(&root, &env, accepted["id"].as_str().unwrap());
    assert_eq!(result["state"], "failed", "{result}");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("environment was replaced")
    );
    assert!(result["result"].is_null());
    let final_calls = ["old", "new"]
        .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default());
    assert_eq!(
        original_calls, final_calls,
        "reassigned request executed runtime I/O"
    );
    assert_eq!(
        command(&root, &env, &["env", "show", "demo", "--json"])["defaultRuntime"],
        "old"
    );
    assert!(
        command(&root, &env, &["upgrade", "history", "demo", "--json"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn conditional_job_rejects_rebinding_after_probe_and_while_waiting() {
    use fs2::FileExt;
    for queued in [false, true] {
        // Check both identity components: another runtime, and a same-name launcher.
        for (kind, name) in [("runtime", "new"), ("launcher", "old")] {
            let root = TestDir::new("upgrade-job-binding");
            let mut env = setup(&root);
            let releases = TestHttpServer::serve_bytes("/openclaw", "application/json", b"{}");
            env.insert("OCM_INTERNAL_OPENCLAW_RELEASES_URL".into(), releases.url());
            command(
                &root,
                &env,
                &[
                    "launcher",
                    "add",
                    "old",
                    "--command",
                    &path_string(&root.child("old")),
                    "--json",
                ],
            );
            let capability = command(&root, &env, &["upgrade", "job", "capabilities", "demo"]);
            let binding = format!(
                "{}:{}",
                capability["bindingKind"].as_str().unwrap(),
                capability["bindingName"].as_str().unwrap()
            );
            let args = [
                "upgrade",
                "job",
                "start",
                "demo",
                "--channel",
                "stable",
                "--request-id",
                "conditional",
                "--if-binding",
                &binding,
            ];
            let locks = root.child("ocm-home/locks/upgrades");
            fs::create_dir_all(&locks).unwrap();
            let transaction = fs::File::create(locks.join("demo.lock")).unwrap();
            transaction.lock_exclusive().unwrap();
            if queued {
                assert_eq!(command(&root, &env, &args)["state"], "running");
            }
            if kind == "runtime" {
                // An explicit runtime transition itself uses the transaction lock.
                // Clear then bind through the ordinary metadata commands instead.
                command(
                    &root,
                    &env,
                    &["env", "set-runtime", "demo", "none", "--json"],
                );
            }
            let rebind = format!("set-{kind}");
            command(&root, &env, &["env", &rebind, "demo", name, "--json"]);
            let rebound = fs::read(root.child("ocm-home/envs.json")).unwrap();
            let original_calls = ["old", "new"]
                .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default());
            if !queued {
                let rejected = run_ocm(root.path(), &env, &args);
                assert!(!rejected.status.success());
                assert!(
                    stderr(&rejected).contains("environment binding changed"),
                    "{}",
                    stderr(&rejected)
                );
                assert!(command(&root, &env, &["upgrade", "job", "status", "demo"]).is_null());
                let jobs = ocm::store::upgrade_history_env_dir("demo", &env, root.path())
                    .unwrap()
                    .join("jobs");
                assert!(!jobs.join("conditional.json").exists());
            }
            transaction.unlock().unwrap();
            if queued {
                let result = wait_for_result(&root, &env, "conditional");
                assert_eq!(result["state"], "failed", "{result}");
                assert!(
                    result["error"]
                        .as_str()
                        .unwrap()
                        .contains("environment binding changed"),
                    "{result}"
                );
                assert!(result["result"].is_null());
                assert_eq!(command(&root, &env, &args), result);
            }
            assert_eq!(fs::read(root.child("ocm-home/envs.json")).unwrap(), rebound);
            assert!(
                releases.requests().is_empty(),
                "rejected job resolved a package channel"
            );
            assert_eq!(
                original_calls,
                ["old", "new"]
                    .map(|name| fs::read(root.child(format!("{name}-calls"))).unwrap_or_default())
            );
            assert!(
                command(&root, &env, &["upgrade", "history", "demo", "--json"])
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[test]
fn unconditional_job_preserves_explicit_launcher_conversion() {
    let root = TestDir::new("upgrade-job-conversion");
    let env = setup(&root);
    command(
        &root,
        &env,
        &[
            "launcher",
            "add",
            "source",
            "--command",
            &path_string(&root.child("old")),
            "--json",
        ],
    );
    command(
        &root,
        &env,
        &["env", "set-launcher", "demo", "source", "--json"],
    );
    fs::write(root.child("release"), "").unwrap();
    let accepted = command(
        &root,
        &env,
        &["upgrade", "job", "start", "demo", "--runtime", "new"],
    );
    let result = wait_for_result(&root, &env, accepted["id"].as_str().unwrap());
    assert_eq!(result["state"], "succeeded", "{result}");
    let record: Value = serde_json::from_slice(
        &fs::read(
            ocm::store::upgrade_history_env_dir("demo", &env, root.path())
                .unwrap()
                .join(format!("jobs/{}.json", accepted["id"].as_str().unwrap())),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(record["schema"], 1);
    assert!(record.get("expectedBinding").is_none());
    let final_env = command(&root, &env, &["env", "show", "demo", "--json"]);
    assert_eq!(final_env["defaultRuntime"], "new");
    assert!(final_env["defaultLauncher"].is_null());
}
