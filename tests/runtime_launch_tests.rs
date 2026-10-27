mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use ocm::env::EnvironmentService;
use ocm::runtime::{InstallRuntimeFromOfficialReleaseOptions, RuntimeService};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use support::{TestDir, TestHttpServer, ocm_env, path_string, run_ocm, stderr, stdout};

const VERSION: &str = "2026.4.25";
const SCRIPT: &str = r#"#!/usr/bin/env node
const args = process.argv.slice(2);
if (args.length === 1 && args[0] === '--version') {
  console.log('2026.4.25');
} else {
  console.log(JSON.stringify({args, executable: process.execPath, cwd: process.cwd(), parent: process.ppid,
    active: process.env.OCM_ACTIVE_ENV, home: process.env.OPENCLAW_HOME,
    marker: process.env.OCM_LAUNCH_MARKER, path: process.env.PATH}));
  console.error('launch-stderr');
  if (args[0] === 'exit-17') process.exitCode = 17;
}
"#;

struct InstalledPackage {
    root: TestDir,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    entrypoint: PathBuf,
    metadata_path: PathBuf,
}

impl InstalledPackage {
    fn new(engine: &str) -> Self {
        let root = TestDir::new("installed package launch");
        let cwd = root.child("caller with spaces");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "unsupported".into());
        env.insert("npm_config_offline".into(), "true".into());
        env.insert("npm_config_audit".into(), "false".into());
        env.insert("npm_config_fund".into(), "false".into());
        env.insert(
            "npm_config_cache".into(),
            path_string(&root.child("npm-cache")),
        );
        env.insert(
            "OCM_LAUNCH_MARKER".into(),
            "marker & %PATH% !literal!".into(),
        );
        #[cfg(windows)]
        {
            for key in ["PATHEXT", "WINDIR", "COMSPEC"] {
                if let Ok(value) = std::env::var(key) {
                    env.insert(key.into(), value);
                }
            }
            for (key, directory) in [
                ("USERPROFILE", "home"),
                ("APPDATA", "appdata"),
                ("LOCALAPPDATA", "localappdata"),
                ("TEMP", "tmp"),
                ("TMP", "tmp"),
            ] {
                fs::create_dir_all(root.child(directory)).unwrap();
                env.insert(key.into(), path_string(&root.child(directory)));
            }
            let npm = Command::new("where.exe").arg("npm.cmd").output().unwrap();
            assert!(npm.status.success(), "{}", stderr(&npm));
            env.insert(
                "OCM_INTERNAL_NPM_BIN".into(),
                stdout(&npm).lines().next().unwrap().trim().into(),
            );
        }
        let manifest = json!({"name":"openclaw", "version":VERSION,
            "bin":{"openclaw":"openclaw.mjs"}, "engines":{"node":engine}})
        .to_string();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = Builder::new(&mut encoder);
            for (name, bytes) in [
                ("package/openclaw.mjs", SCRIPT.as_bytes()),
                ("package/package.json", manifest.as_bytes()),
            ] {
                let mut header = Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                archive.append_data(&mut header, name, bytes).unwrap();
            }
            archive.finish().unwrap();
        }
        let tarball = encoder.finish().unwrap();
        let package =
            TestHttpServer::serve_bytes("/openclaw.tgz", "application/octet-stream", &tarball);
        let integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&tarball))
        );
        let catalog = json!({"dist-tags":{"latest":VERSION}, "versions":{
            VERSION:{"version":VERSION,"dist":{"tarball":package.url(),"integrity":integrity}}
        }})
        .to_string();
        let releases =
            TestHttpServer::serve_bytes("/openclaw", "application/json", catalog.as_bytes());
        env.insert("OCM_INTERNAL_OPENCLAW_RELEASES_URL".into(), releases.url());
        // Publish valid existing state through the real owner API. This fixture
        // isolates launch; it does not stand in for CLI build/publication proof.
        let meta = RuntimeService::new(&env, &cwd)
            .install_from_official_openclaw_release(InstallRuntimeFromOfficialReleaseOptions {
                name: VERSION.into(),
                version: Some(VERSION.into()),
                channel: None,
                description: None,
                force: false,
            })
            .unwrap();
        let metadata_path = ocm::store::runtime_meta_path(VERSION, &env, &cwd).unwrap();
        let create = run_ocm(
            &cwd,
            &env,
            &["env", "create", "native", "--runtime", VERSION],
        );
        assert!(create.status.success(), "{}", stderr(&create));
        Self {
            root,
            cwd,
            env,
            entrypoint: PathBuf::from(meta.binary_path),
            metadata_path,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        run_ocm(&self.cwd, &self.env, args)
    }
}

fn same_path(left: impl AsRef<Path>, right: impl AsRef<Path>) {
    assert_eq!(
        fs::canonicalize(left).unwrap(),
        fs::canonicalize(right).unwrap()
    );
}

#[test]
fn installed_package_launch_preserves_command_identity() {
    // Probe only the expected host identity; installed-package execution below
    // always goes through OCM, never a substitute direct Node invocation.
    let probe = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(probe.status.success(), "{}", stderr(&probe));
    let host_node = stdout(&probe).trim().to_string();
    let fixture = InstalledPackage::new(">=22.22.3");
    let metadata_before = fs::read(&fixture.metadata_path).unwrap();
    let entrypoint_before = fs::read(&fixture.entrypoint).unwrap();

    // Temporary before/after qualification against exactly the same published
    // state. The native job supplies its separately built unchanged-base CLI.
    #[cfg(windows)]
    if let Some(baseline) = std::env::var_os("OCM_TEST_BASELINE_BINARY") {
        let baseline = PathBuf::from(baseline);
        let before = support::run_ocm_binary(
            &baseline,
            &fixture.cwd,
            &fixture.env,
            &["@native", "--", "--version"],
        );
        println!(
            "{}",
            json!({"phase":"installed-launch-before",
            "binarySha256":ocm::infra::download::file_sha256(&baseline).unwrap(),
            "metadataSha256":ocm::infra::download::file_sha256(&fixture.metadata_path).unwrap(),
            "entrypointSha256":ocm::infra::download::file_sha256(&fixture.entrypoint).unwrap(),
            "exit":before.status.code(),"stdout":stdout(&before),"stderr":stderr(&before)})
        );
        assert_eq!(before.status.code(), Some(1));
        assert!(
            stderr(&before).contains("os error 193"),
            "{}",
            stderr(&before)
        );
    }
    let version = fixture.run(&["@native", "--", "--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version).trim(), VERSION);
    println!(
        "{}",
        json!({"phase":"installed-launch-after",
        "binarySha256":ocm::infra::download::file_sha256(&support::ocm_test_binary_path()).unwrap(),
        "metadataSha256":ocm::infra::download::file_sha256(&fixture.metadata_path).unwrap(),
        "entrypointSha256":ocm::infra::download::file_sha256(&fixture.entrypoint).unwrap(),
        "exit":version.status.code(),"stdout":stdout(&version),"stderr":stderr(&version)})
    );

    let forwarded = [
        "echo",
        "two words",
        "a\"b",
        "&|;%PATH%!literal!",
        "雪",
        "",
        "trailing\\",
    ];
    let mut args = vec!["env", "run", "native", "--"];
    args.extend(forwarded);
    let child = Command::new(support::ocm_test_binary_path())
        .current_dir(&fixture.cwd)
        .env_clear()
        .envs(&fixture.env)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ocm_pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["args"], json!(forwarded));
    assert_eq!(value["parent"], ocm_pid);
    assert_eq!(stderr(&output).trim(), "launch-stderr");
    same_path(value["executable"].as_str().unwrap(), &host_node);
    same_path(value["cwd"].as_str().unwrap(), &fixture.cwd);
    same_path(
        value["home"].as_str().unwrap(),
        fixture.root.child("ocm-home/envs/native"),
    );
    assert_eq!(value["active"], "native");
    assert_eq!(value["marker"], fixture.env["OCM_LAUNCH_MARKER"]);
    assert_eq!(value["path"], fixture.env["PATH"]);
    assert_eq!(
        fixture.run(&["@native", "--", "exit-17"]).status.code(),
        Some(17)
    );

    let service = EnvironmentService::new(&fixture.env, &fixture.cwd);
    let forwarded = forwarded.map(str::to_string);
    let summary = service
        .resolve("native", None, None, &forwarded)
        .unwrap()
        .into_summary();
    same_path(summary.binary_path.unwrap(), &fixture.entrypoint);
    assert_eq!(summary.forwarded_args, forwarded);
    let gateway = service.resolve_gateway_process("native", false).unwrap();
    assert!(gateway.restart_handoff_pid_bound());
    same_path(&gateway.run_dir, fixture.root.child("ocm-home/envs/native"));
    if cfg!(windows) {
        assert_eq!(gateway.binary_path.as_deref(), Some("node"));
        same_path(&gateway.args[0], &fixture.entrypoint);
        assert_eq!(&gateway.args[1..3], &["gateway", "run"]);
    } else {
        same_path(gateway.binary_path.as_ref().unwrap(), &fixture.entrypoint);
        assert_eq!(&gateway.args[..2], &["gateway", "run"]);
    }
    assert!(!fixture.root.child("ocm-home/toolchains/node").exists());
    assert_eq!(fs::read(&fixture.metadata_path).unwrap(), metadata_before);
    assert_eq!(fs::read(&fixture.entrypoint).unwrap(), entrypoint_before);

    // Registered and installed custom executables retain direct execution.
    for (name, operation) in [("registered-node", "add"), ("installed-node", "install")] {
        let add = fixture.run(&["runtime", operation, name, "--path", &host_node]);
        assert!(add.status.success(), "{}", stderr(&add));
        let create = fixture.run(&["env", "create", name, "--runtime", name]);
        assert!(create.status.success(), "{}", stderr(&create));
        let version = fixture.run(&["env", "run", name, "--", "--version"]);
        assert!(version.status.success(), "{}", stderr(&version));
        assert!(stdout(&version).trim().starts_with('v'));
        let spec = service.resolve_gateway_process(name, false).unwrap();
        assert!(!spec.restart_handoff_pid_bound());
        assert_eq!(&spec.args[..2], &["gateway", "run"]);
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires the declared Windows Node 22 host and pinned managed Node download"]
fn installed_package_launch_keeps_managed_fallback() {
    let host = Command::new("node").arg("--version").output().unwrap();
    assert!(host.status.success());
    assert!(stdout(&host).starts_with("v22."), "{}", stdout(&host));
    let mut fixture = InstalledPackage::new(">=24.22.1 <25");
    fixture.env.insert(
        "OCM_INTERNAL_MANAGED_NODE_ARCHIVE_URL".into(),
        "http://127.0.0.1:1/unavailable.zip".into(),
    );
    // Both incompatible-host and missing-host paths reuse the valid private
    // toolchain prepared by package installation, with downloads unavailable.
    for missing in [false, true] {
        if missing {
            fixture.env.insert("PATH".into(), String::new());
        }
        let output = fixture.run(&["@native", "--", "inspect"]);
        assert!(output.status.success(), "{}", stderr(&output));
        let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
        let executable = PathBuf::from(value["executable"].as_str().unwrap());
        assert!(fs::canonicalize(&executable).unwrap().starts_with(
            fs::canonicalize(fixture.root.child("ocm-home/toolchains/node")).unwrap()
        ));
        assert_eq!(value["args"], json!(["inspect"]));
        assert_eq!(value["active"], "native");
        assert_eq!(value["marker"], fixture.env["OCM_LAUNCH_MARKER"]);
    }
}
