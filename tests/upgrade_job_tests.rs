#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::{TestDir, ocm_env, path_string, run_ocm, stderr, stdout, write_executable_script};

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
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = command(
            root,
            env,
            &["upgrade", "job", "status", "demo", "--request-id", id],
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
fn detached_upgrade_preserves_exclusion_and_each_requests_result() {
    let root = TestDir::new("upgrade-job-results");
    let env = setup(&root);
    let capabilities = command(&root, &env, &["upgrade", "job", "capabilities", "demo"]);
    assert_eq!(capabilities["protocol"], "ocm.upgrade-job");
    assert_eq!(capabilities["protocolVersion"], 1);
    assert_eq!(capabilities["supported"], true);
    assert_eq!(capabilities["envName"], "demo");
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
            "--json",
        ],
    );
    let id = accepted["id"].as_str().unwrap();
    assert_eq!(accepted["state"], "running");
    assert!(accepted["result"].is_null());
    assert_eq!(accepted["envName"], "demo");
    assert_eq!(accepted["id"], "admission");
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
}

#[test]
fn lost_worker_reports_unresolved_interruption_without_replay() {
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
    // SAFETY: this fixture owns the detached worker's entire process group.
    assert_eq!(unsafe { libc::kill(-pid, libc::SIGKILL) }, 0);
    let status = wait_for_result(&root, &env, id);
    assert_eq!(status["state"], "interrupted");
    assert!(status["result"].is_null());
    assert!(
        status["error"]
            .as_str()
            .unwrap()
            .contains("recovery is unresolved")
    );
    let retry = run_ocm(
        root.path(),
        &env,
        &["upgrade", "job", "start", "demo", "--runtime", "new"],
    );
    assert!(!retry.status.success());
    assert!(stderr(&retry).contains("Interrupted"), "{}", stderr(&retry));
    let history = command(&root, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.as_array().unwrap().is_empty());
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
