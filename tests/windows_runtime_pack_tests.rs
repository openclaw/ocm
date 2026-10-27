#![cfg(windows)]

mod support;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use support::{
    TestDir, ocm_env, ocm_test_binary_path, path_string, run_ocm, stderr, windows_job::FixtureJob,
};

#[test]
fn native_windows_pack_preserves_live_child_archive() {
    let fixture = TestDir::new("native-windows-runtime-pack");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output_path = fixture.child("proof.log");
    let output = fs::File::create(&output_path).unwrap();
    let mut command = Command::new("node");
    command
        .current_dir(repo)
        .arg(repo.join("tests/support/windows_runtime_pack.cjs"))
        .arg(ocm_test_binary_path())
        .arg(fixture.path())
        .env_clear()
        .envs(ocm_env(&fixture))
        .stdin(Stdio::piped())
        .stdout(output.try_clone().unwrap())
        .stderr(output);
    for key in ["PATH", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("TEMP", fixture.path())
        .env("TMP", fixture.path());
    let job = FixtureJob::new().expect("create private packing fixture job");
    let mut child = command.spawn().expect("start native packing fixture");
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{error}");
    }
    // The helper must not spawn OCM until all descendants belong to our Job.
    child.stdin.take().unwrap().write_all(b"owned\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error.to_string()),
            Ok(None) if Instant::now() >= deadline => break Err("packing fixture timed out".into()),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Parent-only termination is part of the scenario. Reap every surviving
    // descendant only after its assertions, including on test failure.
    job.stop().expect("settle every packing fixture process");
    let _ = child.wait();
    let diagnostics = fs::read_to_string(output_path).unwrap();
    println!("{diagnostics}");
    assert!(
        matches!(&result, Ok(status) if status.success()),
        "{result:?}\n{diagnostics}"
    );

    // The original active-child assertion ran before Job cleanup. With all
    // descendants now proven stopped, retry must reclaim its abandoned stage.
    let mut env = ocm_env(&fixture);
    for key in ["WINDIR", "COMSPEC", "PATHEXT"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.into(), value);
        }
    }
    env.insert("USERPROFILE".into(), path_string(&fixture.child("home")));
    env.insert("APPDATA".into(), path_string(&fixture.child("appdata")));
    env.insert(
        "LOCALAPPDATA".into(),
        path_string(&fixture.child("localappdata")),
    );
    env.insert("TEMP".into(), path_string(fixture.path()));
    env.insert("TMP".into(), path_string(fixture.path()));
    env.insert("npm_config_offline".into(), "true".into());
    env.insert(
        "npm_config_cache".into(),
        path_string(&fixture.child("cache")),
    );
    env.insert(
        "OCM_INTERNAL_NPM_BIN".into(),
        fs::read_to_string(fixture.child("npm-bin")).unwrap(),
    );
    let source = path_string(&fixture.child("repo"));
    let args = [
        "runtime",
        "build-local",
        "stable",
        "--repo",
        &source,
        "--force",
        "--json",
    ];
    let retry = run_ocm(fixture.path(), &env, &args);
    assert!(retry.status.success(), "{}", stderr(&retry));
    let runtimes = fixture.child("ocm-home/runtimes");
    let no_stages = || {
        fs::read_dir(&runtimes)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| {
                !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".stable.stage-")
            })
    };
    assert!(no_stages());
    assert!(!runtimes.join("stable/pack").exists());
    let verify = run_ocm(
        fixture.path(),
        &env,
        &["runtime", "verify", "stable", "--json"],
    );
    assert!(verify.status.success(), "{}", stderr(&verify));
    env.insert("PACK_FAIL".into(), "1".into());
    let failure = run_ocm(fixture.path(), &env, &args);
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("failed to pack local OpenClaw build"));
    assert!(no_stages());
    let verify = run_ocm(
        fixture.path(),
        &env,
        &["runtime", "verify", "stable", "--json"],
    );
    assert!(verify.status.success(), "{}", stderr(&verify));
    let remove = run_ocm(
        fixture.path(),
        &env,
        &["runtime", "remove", "stable", "--json"],
    );
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(no_stages());
    println!("native Windows retry, publication, error preservation and removal passed");
}
