#![cfg(unix)]
mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use flate2::{Compression, write::GzEncoder};
use ocm::infra::download::file_sha256;
use serde_json::{Value, json};
use support::{TestDir, TestHttpServer, ocm_env, run_ocm_binary, stderr, write_executable_script};

fn wait_for(mut predicate: impl FnMut() -> bool, detail: &str) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        sleep(Duration::from_millis(50));
    }
    panic!("timed out: {detail}");
}

fn read_json(path: &Path) -> Value {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or(Value::Null)
}

// Compile the same source as a different release. No patched version strings,
// fake runtime-version records, shared target directory, or external downloads.
fn build_candidate(root: &TestDir) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.child("candidate-package");
    fs::create_dir_all(&package).unwrap();
    let manifest = fs::read_to_string(source.join("Cargo.toml"))
        .unwrap()
        .replacen(
            &format!("version = \"{}\"", env!("CARGO_PKG_VERSION")),
            "version = \"9.9.9\"",
            1,
        );
    fs::write(
        package.join("Cargo.toml"),
        format!(
            "{manifest}\n[lib]\npath = {:?}\n[[bin]]\nname = \"ocm\"\npath = {:?}\n",
            source.join("src/lib.rs"),
            source.join("src/main.rs")
        ),
    )
    .unwrap();
    let lock = fs::read_to_string(source.join("Cargo.lock"))
        .unwrap()
        .replace(
            &format!(
                "name = \"ocm\"\nversion = \"{}\"",
                env!("CARGO_PKG_VERSION")
            ),
            "name = \"ocm\"\nversion = \"9.9.9\"",
        );
    fs::write(package.join("Cargo.lock"), lock).unwrap();
    let output = Command::new(env!("CARGO"))
        .args(["build", "--locked", "--offline", "--bin", "ocm"])
        .current_dir(&package)
        .env("CARGO_TARGET_DIR", package.join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "candidate build: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    package.join("target/debug/ocm")
}

struct Fixture {
    root: TestDir,
    env: BTreeMap<String, String>,
    binary: PathBuf,
    _release: TestHttpServer,
    _asset: TestHttpServer,
}

impl Fixture {
    fn new(candidate: &Path, label: &str) -> Self {
        let root = TestDir::new(label);
        let mut env = ocm_env(&root);
        env.remove("OCM_INTERNAL_SKIP_SERVICE_READINESS");
        env.insert("OCM_INTERNAL_SELF_UPDATE_TIMEOUT_MS".into(), "2000".into());
        env.insert(
            "OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS".into(),
            "3000".into(),
        );
        let binary = root.child("bin/ocm");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_ocm"), &binary).unwrap();
        let archive = root.child("candidate.tar.gz");
        let file = fs::File::create(&archive).unwrap();
        let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::fast()));
        tar.append_path_with_name(candidate, "ocm").unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let asset = TestHttpServer::serve_bytes_times(
            "/candidate",
            "application/gzip",
            &fs::read(&archive).unwrap(),
            4,
        );
        let target = if cfg!(target_os = "macos") {
            "apple-darwin"
        } else {
            "unknown-linux-gnu"
        };
        let metadata = json!({"tag_name":"v9.9.9", "assets":[{
            "name":format!("ocm-{}-{target}.tar.gz", std::env::consts::ARCH),
            "browser_download_url":asset.url(),
            "digest":format!("sha256:{}", file_sha256(&archive).unwrap())
        }]});
        let release = TestHttpServer::serve_bytes_times(
            "/release",
            "application/json",
            &serde_json::to_vec(&metadata).unwrap(),
            8,
        );
        env.insert("OCM_INTERNAL_SELF_UPDATE_RELEASE_URL".into(), release.url());
        env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
        let manager = root.child("manager");
        env.insert(
            "OCM_INTERNAL_LAUNCHCTL_BIN".into(),
            manager.to_string_lossy().into_owned(),
        );
        // This is a process-launching stand-in, not a mock of daemon/runtime data.
        // It never invokes the host service manager. Each PID comes from spawn.
        let script = format!(
            r##"#!/usr/bin/env node
const fs = require('node:fs');
const cp = require('node:child_process');
const root = {root};
const binary = {binary};
const pidFile = root + '/daemon.pid';
const args = process.argv.slice(2);
const sleep = ms => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
const alive = pid => {{ try {{ process.kill(pid, 0); return true; }} catch {{ return false; }} }};
fs.appendFileSync(root + '/manager.log', args.join(' ') + '\n');
if (args[0] === 'managername') {{ console.log('Aqua'); process.exit(0); }}
if (args[0] === 'print-disabled' || args[0] === 'enable') process.exit(0);
if (args[0] === 'print') {{
  if (/^(gui|user)\/[0-9]+$/.test(args[1])) process.exit(0);
  const pid = fs.existsSync(pidFile) ? Number(fs.readFileSync(pidFile, 'utf8')) : 0;
  if (pid && alive(pid)) {{ console.log('state = running\npid = ' + pid); process.exit(0); }}
  console.error('Could not find service'); process.exit(1);
}}
if (args[0] === 'bootout' || args[0] === 'unload') {{
  if (fs.existsSync(pidFile)) {{
    const pid = Number(fs.readFileSync(pidFile, 'utf8'));
    if (alive(pid)) process.kill(pid, 'SIGTERM');
    for (let i = 0; i < 150 && alive(pid); i++) sleep(20);
    fs.unlinkSync(pidFile);
  }}
  process.exit(0);
}}
if (args[0] === 'bootstrap') {{
  if (fs.existsSync(root + '/pause')) {{
    fs.writeFileSync(root + '/worker.pid', String(process.ppid));
    fs.writeFileSync(root + '/manager.pid', String(process.pid));
    while (fs.existsSync(root + '/pause')) sleep(20);
  }}
  const version = cp.execFileSync(binary, ['--version'], {{encoding:'utf8'}}).trim();
  if (version === '9.9.9' && fs.existsSync(root + '/fail-candidate')) {{
    console.error('injected candidate activation failure'); process.exit(1);
  }}
  if (fs.existsSync(root + '/fail-all')) {{ console.error('injected persistent failure'); process.exit(1); }}
  const log = fs.openSync(root + '/daemon.log', 'a');
  const child = cp.spawn(binary, ['__daemon', 'run'], {{env: {env}, detached:true, stdio:['ignore',log,log]}});
  fs.writeFileSync(pidFile, String(child.pid)); child.unref(); process.exit(0);
}}
process.exit(0);
"##,
            root = json!(root.path()),
            binary = json!(binary),
            env = json!(env)
        );
        write_executable_script(&manager, &script);
        let probe = Command::new(&manager)
            .arg("managername")
            .env_clear()
            .envs(&env)
            .output()
            .unwrap();
        assert!(
            probe.status.success(),
            "fixture manager: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        Self {
            root,
            env,
            binary,
            _release: release,
            _asset: asset,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        run_ocm_binary(&self.binary, self.root.path(), &self.env, args)
    }

    fn receipt(&self) -> Value {
        read_json(&self.root.child("bin/.ocm.self-update/receipt.json"))
    }

    fn setup_gateway(&self) {
        let gateway = self.root.child("gateway");
        write_executable_script(
            &gateway,
            &format!(
                r##"#!/usr/bin/env node
const fs = require('node:fs');
const cp = require('node:child_process');
const http = require('node:http');
const args = process.argv.slice(2);
const root = {root};
if (args[0] === '--version') {{ console.log('2026.8.1'); process.exit(0); }}
if (args[1] === 'restart-handoff') process.exit(64);
if (args[1] === 'status') {{ console.log('{{"rpc":{{"ok":true}}}}'); process.exit(0); }}
if (args[0] !== 'gateway' || !args.includes('--port')) process.exit(0);
fs.appendFileSync(root + '/gateway-starts', process.pid + '\n');
const port = Number(args[args.indexOf('--port') + 1]);
http.createServer((req,res) => {{ res.writeHead(200); res.end('ok'); }}).listen(port, '127.0.0.1');
setInterval(() => {{
  if (fs.existsSync(root + '/invoke') && !fs.existsSync(root + '/invoked')) {{
    fs.writeFileSync(root + '/invoked', 'yes');
    const log = fs.openSync(root + '/update.log', 'a');
    const child = cp.spawn({binary}, ['self','update','--version','9.9.9','--json'], {{env:{env},stdio:['ignore',log,log]}});
    fs.writeFileSync(root + '/caller.pid', String(child.pid));
  }}
}}, 50);
"##,
                root = json!(self.root.path()),
                binary = json!(self.binary),
                env = json!(self.env)
            ),
        );
        for args in [
            vec![
                "runtime",
                "add",
                "fixture",
                "--path",
                gateway.to_str().unwrap(),
            ],
            vec!["env", "create", "running", "--runtime", "fixture"],
            vec!["env", "create", "stopped", "--runtime", "fixture"],
            vec!["service", "start", "running"],
        ] {
            let result = self.run(&args);
            assert!(result.status.success(), "{args:?}: {}", stderr(&result));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Exact owned PID, from this fixture's manager. Graceful daemon shutdown
        // also reaps its gateway group; no host service or unrelated process.
        if let Ok(pid) = fs::read_to_string(self.root.child("daemon.pid")) {
            let _ = Command::new("kill").args(["-TERM", pid.trim()]).status();
            sleep(Duration::from_millis(400));
        }
    }
}

#[test]
fn local_transaction_survives_gateway_teardown_and_recovers_failures() {
    let candidate_root = TestDir::new("self-update-build");
    let candidate = build_candidate(&candidate_root);

    // The source gateway launches the update in its own group. Refresh kills
    // that group, including the CLI. Only the detached retained helper survives.
    let success = Fixture::new(&candidate, "transaction-success");
    success.setup_gateway();
    let before = success.run(&["service", "status", "running", "--json"]);
    let before: Value = serde_json::from_slice(&before.stdout).unwrap();
    let gateway_pid = before["childPid"].as_u64().unwrap() as i32;
    fs::write(success.root.child("invoke"), "go").unwrap();
    wait_for(
        || success.receipt()["phase"] == "updated",
        "gateway-owned update receipt",
    );
    let receipt = success.receipt();
    assert_eq!(receipt["daemonWasRunning"], true);
    assert_eq!(receipt["gateways"], json!(["running"]));
    assert_eq!(
        String::from_utf8(success.run(&["--version"]).stdout)
            .unwrap()
            .trim(),
        "9.9.9"
    );
    let status = success.run(&["service", "status", "running", "--json"]);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["ocmServiceVersion"], "9.9.9");
    assert_eq!(status["running"], true);
    assert_ne!(status["childPid"], gateway_pid);
    // SAFETY: signal 0 observes only the fixture's original process group.
    assert_eq!(
        unsafe { libc::kill(-gateway_pid, 0) },
        -1,
        "source process group survived"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(
            &success
                .run(&["service", "status", "stopped", "--json"])
                .stdout
        )
        .unwrap()["desiredRunning"],
        false
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&success.run(&["self", "update", "--status"]).stdout)
            .unwrap(),
        receipt
    );
    let manager_before = fs::read(success.root.child("manager.log")).unwrap();
    assert!(
        success
            .run(&["self", "update", "--version", "9.9.9", "--json"])
            .status
            .success()
    );
    assert_eq!(
        fs::read(success.root.child("manager.log")).unwrap(),
        manager_before,
        "unchanged release touched service manager"
    );
    drop(success);

    let rollback = Fixture::new(&candidate, "transaction-rollback");
    rollback.setup_gateway();
    let original = file_sha256(&rollback.binary).unwrap();
    fs::write(rollback.root.child("fail-candidate"), "yes").unwrap();
    let result = rollback.run(&["self", "update", "--version", "9.9.9", "--json"]);
    assert!(
        !result.status.success(),
        "failed activation was reported as successful"
    );
    assert_eq!(
        rollback.receipt()["phase"],
        "rolledBack",
        "{}",
        stderr(&result)
    );
    assert_eq!(file_sha256(&rollback.binary).unwrap(), original);
    assert!(
        rollback.receipt()["error"]
            .as_str()
            .unwrap()
            .contains("injected candidate activation failure")
    );
    let status: Value = serde_json::from_slice(
        &rollback
            .run(&["service", "status", "running", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(status["ocmServiceVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["running"], true);
    drop(rollback);

    let crash = Fixture::new(&candidate, "transaction-crash");
    crash.setup_gateway();
    let original = file_sha256(&crash.binary).unwrap();
    fs::write(crash.root.child("pause"), "yes").unwrap();
    let mut caller = Command::new(&crash.binary)
        .args(["self", "update", "--version", "9.9.9", "--json"])
        .env_clear()
        .envs(&crash.env)
        .current_dir(crash.root.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(
        || crash.root.child("worker.pid").exists(),
        "worker reached post-publication pause",
    );
    assert_eq!(crash.receipt()["phase"], "applying");
    let id = crash.receipt()["id"].clone();
    let concurrent = crash.run(&["self", "update", "--version", "9.9.9"]);
    assert!(!concurrent.status.success());
    assert!(stderr(&concurrent).contains("self-update is busy"));
    let recover_busy = crash.run(&["self", "update", "--recover"]);
    assert!(!recover_busy.status.success());
    let worker_pid = fs::read_to_string(crash.root.child("worker.pid")).unwrap();
    assert!(
        Command::new("kill")
            .args(["-KILL", worker_pid.trim()])
            .status()
            .unwrap()
            .success()
    );
    assert!(!caller.wait().unwrap().success());
    // A still-running activation command retains the update lock after its
    // worker dies. Releasing the fixture pause lets that command finish.
    let orphan_busy = crash.run(&["self", "update", "--recover"]);
    assert!(!orphan_busy.status.success());
    assert!(stderr(&orphan_busy).contains("self-update is busy"));
    fs::remove_file(crash.root.child("pause")).unwrap();
    wait_for(
        || crash.root.child("daemon.pid").exists(),
        "orphan activation completed",
    );
    sleep(Duration::from_millis(200));
    let retry = crash.run(&["self", "update", "--version", "9.9.9"]);
    assert!(!retry.status.success());
    assert!(stderr(&retry).contains("unfinished self-update"));
    let recovered = crash.run(&["self", "update", "--recover"]);
    assert_eq!(
        crash.receipt()["phase"],
        "rolledBack",
        "{}",
        stderr(&recovered)
    );
    assert_eq!(crash.receipt()["id"], id);
    assert_eq!(file_sha256(&crash.binary).unwrap(), original);
    let before_retry = fs::read(crash.root.child("manager.log")).unwrap();
    let _ = crash.run(&["self", "update", "--recover"]);
    assert_eq!(
        fs::read(crash.root.child("manager.log")).unwrap(),
        before_retry,
        "recovery replay changed services"
    );
    drop(crash);

    let failed_rollback = Fixture::new(&candidate, "transaction-rollback-failed");
    failed_rollback.setup_gateway();
    fs::write(failed_rollback.root.child("fail-all"), "yes").unwrap();
    let failure = failed_rollback.run(&["self", "update", "--version", "9.9.9"]);
    assert!(!failure.status.success());
    assert_eq!(failed_rollback.receipt()["phase"], "rollbackFailed");
    assert!(failed_rollback.receipt()["rollbackError"].is_string());
    assert!(
        failed_rollback
            .root
            .child("bin/.ocm.self-update/previous")
            .is_file()
    );
    fs::remove_file(failed_rollback.root.child("fail-all")).unwrap();
    let _ = failed_rollback.run(&["self", "update", "--recover"]);
    assert_eq!(failed_rollback.receipt()["phase"], "rolledBack");
    assert_eq!(failed_rollback.receipt()["rollbackError"], Value::Null);
}
