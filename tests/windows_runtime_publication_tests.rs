#![cfg(windows)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

use support::{
    TestDir, TestHttpServer, ocm_env, path_string, run_ocm, sha512_integrity, stderr, stdout,
};

const OLD: &str = "2026.10.8";
const NEW: &str = "2026.10.9";

struct NativeFixture {
    root: TestDir,
    env: BTreeMap<String, String>,
    node: PathBuf,
    npm_cli: PathBuf,
}

struct PackageSource {
    root: PathBuf,
    manifest: Vec<u8>,
    entry: Vec<u8>,
}

struct Release {
    version: String,
    source: PackageSource,
    archive: TestHttpServer,
    integrity: String,
}

impl NativeFixture {
    fn new(label: &str, strategy: &str) -> Self {
        let root = TestDir::new(label);
        let mut env = ocm_env(&root);
        for (key, value) in std::env::vars() {
            if matches!(
                key.to_ascii_uppercase().as_str(),
                "PATH" | "SYSTEMROOT" | "WINDIR" | "COMSPEC" | "PATHEXT"
            ) {
                env.insert(key, value);
            }
        }
        for (key, relative) in [
            ("USERPROFILE", "home"),
            ("APPDATA", "home/AppData/Roaming"),
            ("LOCALAPPDATA", "home/AppData/Local"),
            ("TEMP", "tmp"),
            ("TMP", "tmp"),
            ("npm_config_cache", "npm-cache"),
        ] {
            let path = root.child(relative);
            fs::create_dir_all(&path).unwrap();
            env.insert(key.into(), path_string(&path));
        }
        for (key, value) in [
            ("npm_config_install_strategy", strategy),
            ("npm_config_offline", "true"),
            ("npm_config_audit", "false"),
            ("npm_config_fund", "false"),
            ("npm_config_update_notifier", "false"),
        ] {
            env.insert(key.into(), value.into());
        }
        env.insert(
            "OCM_TEST_NATIVE_PUBLICATION_LOG".into(),
            path_string(&root.child("commands.jsonl")),
        );
        let locate = |name| {
            let output = Command::new("where.exe").arg(name).output().unwrap();
            assert_success(&output, name);
            PathBuf::from(stdout(&output).lines().next().unwrap().trim())
        };
        let node = locate("node.exe");
        let npm = locate("npm.cmd");
        let npm_cli = npm
            .parent()
            .unwrap()
            .join("node_modules/npm/bin/npm-cli.js");
        assert!(npm_cli.is_file(), "native npm CLI missing: {npm_cli:?}");
        env.insert("OCM_INTERNAL_NPM_BIN".into(), path_string(&npm));
        let fixture = Self {
            root,
            env,
            node,
            npm_cli,
        };
        let executable = support::ocm_test_binary_path();
        println!(
            "native identity {}",
            json!({
                "executable": executable,
                "sha256": ocm::infra::download::file_sha256(&executable).unwrap(),
                "node": fixture.node,
                "npmCli": fixture.npm_cli,
            })
        );
        println!("native Node: {}", fixture.node(&["--version"]).trim());
        println!(
            "native npm: {}",
            fixture.npm(fixture.root.path(), &["--version"]).trim()
        );
        fixture
    }

    fn command(&self, program: &Path, cwd: &Path, args: &[&str]) -> Output {
        Command::new(program)
            .current_dir(cwd)
            .env_clear()
            .envs(&self.env)
            .args(args)
            .output()
            .unwrap()
    }

    fn node(&self, args: &[&str]) -> String {
        let output = self.command(&self.node, self.root.path(), args);
        assert_success(&output, "native Node");
        stdout(&output)
    }

    fn npm(&self, cwd: &Path, args: &[&str]) -> String {
        let cli = path_string(&self.npm_cli);
        let mut argv = vec![cli.as_str()];
        argv.extend_from_slice(args);
        let output = self.command(&self.node, cwd, &argv);
        assert_success(&output, "native npm");
        stdout(&output)
    }

    fn ocm(&self, args: &[&str]) -> Output {
        let output = run_ocm(self.root.path(), &self.env, args);
        println!(
            "native receipt {}",
            json!({
                "executable": support::ocm_test_binary_path(), "argv": args,
                "exit": output.status.code(), "stdout": stdout(&output), "stderr": stderr(&output),
            })
        );
        output
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.ocm(args);
        assert_success(&output, &args.join(" "));
        stdout(&output)
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(args)).unwrap()
    }

    fn source(&self, version: &str) -> PackageSource {
        let root = self.root.child(format!("source-{version}"));
        fs::create_dir_all(&root).unwrap();
        let manifest = serde_json::to_vec(&json!({
            "name": "openclaw", "version": version, "type": "module",
            "engines": {"node": ">=22.22.3"},
            "bin": {"openclaw": "openclaw.mjs"}, "files": ["openclaw.mjs"]
        }))
        .unwrap();
        // This host-only package has no dependencies, companions or service. Its
        // protocol responses let the real upgrade command exercise publication.
        let entry = format!(
            r#"import fs from 'node:fs';
const version = {version:?};
const args = process.argv.slice(2);
if (process.env.OCM_TEST_NATIVE_PUBLICATION_LOG) {{
  fs.appendFileSync(process.env.OCM_TEST_NATIVE_PUBLICATION_LOG,
    JSON.stringify({{version, args, entry: import.meta.url}}) + '\n');
}}
if (args[0] === '--version') {{ console.log(version); }}
else if (args[0] === '--payload') {{ console.log('host-bytes-' + version); }}
else if (args[0] === 'config' && args[1] === 'validate') {{ console.log('Config valid'); }}
else if (args[0] === 'doctor' && args[1] === '--lint') {{
  console.log(JSON.stringify({{ok:false, checksRun:0, checksSkipped:1, findings:[{{
    checkId:'core/doctor/lint-selection', severity:'error',
    message:'Unknown health check id selected by --only: codex/managed-app-server.',
    path:'codex/managed-app-server'
  }}]}}));
  process.exitCode = 1;
}}
else if (args[0] === 'update' && args[1] === 'finalize') {{
  if (!['--json', '--yes', '--no-restart'].every(flag => args.includes(flag)) ||
      process.env.OPENCLAW_UPDATE_IN_PROGRESS !== '1') {{
    throw new Error('missing upgrade finalizer contract');
  }}
  if (process.env.OCM_TEST_NATIVE_FAIL_FINALIZE === version) {{
    console.error('native fixture finalizer failure'); process.exitCode = 23;
  }} else {{
    console.log(JSON.stringify({{status:'ok', mode:'finalize',
      postUpdate:{{doctor:{{status:'ok'}}, plugins:{{status:'ok'}}}}}}));
  }}
}}
else {{ throw new Error('unexpected fixture command: ' + JSON.stringify(args)); }}
"#
        )
        .into_bytes();
        fs::write(root.join("package.json"), &manifest).unwrap();
        fs::write(root.join("openclaw.mjs"), &entry).unwrap();
        PackageSource {
            root,
            manifest,
            entry,
        }
    }

    fn pack(&self, source: &PackageSource) -> PathBuf {
        let archives = self.root.child("archives");
        fs::create_dir_all(&archives).unwrap();
        let packed: Value = serde_json::from_str(&self.npm(
            &source.root,
            &[
                "pack",
                "--json",
                "--ignore-scripts",
                "--pack-destination",
                &path_string(&archives),
            ],
        ))
        .unwrap();
        source.assert_unchanged();
        archives.join(packed[0]["filename"].as_str().unwrap())
    }

    fn release(&self, version: &str) -> Release {
        let source = self.source(version);
        let bytes = fs::read(self.pack(&source)).unwrap();
        let archive = TestHttpServer::serve_bytes_times(
            &format!("/openclaw-{version}.tgz"),
            "application/octet-stream",
            &bytes,
            8,
        );
        Release {
            version: version.into(),
            source,
            archive,
            integrity: sha512_integrity(&bytes),
        }
    }

    fn registry(&mut self, old: &Release, new: &Release) -> TestHttpServer {
        let packument =
            |latest: &str| {
                let mut versions = serde_json::Map::new();
                for release in [old, new] {
                    versions.insert(release.version.clone(), json!({
                    "name": "openclaw", "version": release.version,
                    "dist": {"tarball": release.archive.url(), "integrity": release.integrity}
                }));
                }
                serde_json::to_vec(&json!({"dist-tags": {"latest": latest}, "versions": versions}))
                    .unwrap()
            };
        let mut bodies = vec![packument(OLD)];
        bodies.extend(vec![packument(NEW); 12]);
        let registry =
            TestHttpServer::serve_bytes_sequence("/openclaw", "application/json", bodies);
        self.env
            .insert("OCM_INTERNAL_OPENCLAW_RELEASES_URL".into(), registry.url());
        registry
    }

    fn metadata_bytes(&self, name: &str) -> Vec<u8> {
        fs::read(self.root.child(format!("ocm-home/runtimes/{name}.json"))).unwrap()
    }

    fn assert_runtime(&self, name: &str, version: &str, linked: bool) -> Value {
        let before = self.metadata_bytes(name);
        let metadata = self.json(&["runtime", "show", name, "--json"]);
        let verified = self.json(&["runtime", "verify", name, "--json"]);
        assert_eq!(verified["healthy"], true, "{verified}");
        let binary = metadata["binaryPath"].as_str().unwrap();
        assert_eq!(self.node(&[binary, "--version"]).trim(), version);
        assert_eq!(
            self.node(&[binary, "--payload"]).trim(),
            format!("host-bytes-{version}")
        );
        let install = Path::new(metadata["installRoot"].as_str().unwrap());
        let package = install.join("files/node_modules/openclaw");
        if linked {
            let target =
                fs::read_link(&package).expect("npm linked root must remain a native junction");
            let target_text = path_string(&target);
            assert!(
                !target_text.contains(".stage-"),
                "staging target: {target_text}"
            );
            assert!(
                !target_text.contains(".backup-"),
                "backup target: {target_text}"
            );
            assert!(
                fs::canonicalize(&package)
                    .unwrap()
                    .starts_with(fs::canonicalize(install).unwrap())
            );
        } else {
            assert!(
                fs::read_link(&package).is_err(),
                "hoisted control unexpectedly linked"
            );
        }
        assert_eq!(
            self.metadata_bytes(name),
            before,
            "readers rewrote runtime metadata"
        );
        metadata
    }

    fn bind(&self, name: &str, runtime: &str, version: &str) {
        self.ok(&["env", "create", name, "--runtime", runtime]);
        self.assert_binding(name, runtime, version);
    }

    fn assert_binding(&self, name: &str, runtime: &str, version: &str) {
        let env = self.json(&["env", "show", name, "--json"]);
        assert_eq!(env["defaultRuntime"], runtime);
        assert_eq!(env["serviceEnabled"], false);
        assert_eq!(env["serviceRunning"], false);
        assert_eq!(
            self.ok(&[&format!("@{name}"), "--", "--version"]).trim(),
            version
        );
        assert_eq!(
            self.ok(&[&format!("@{name}"), "--", "--payload"]).trim(),
            format!("host-bytes-{version}")
        );
    }
}

impl PackageSource {
    fn assert_unchanged(&self) {
        assert_eq!(
            fs::read(self.root.join("package.json")).unwrap(),
            self.manifest
        );
        assert_eq!(
            fs::read(self.root.join("openclaw.mjs")).unwrap(),
            self.entry
        );
        assert_eq!(
            fs::read_dir(&self.root).unwrap().count(),
            2,
            "source tree was modified"
        );
    }
}

fn assert_success(output: &Output, command: &str) {
    assert!(
        output.status.success(),
        "{command}: {}\n{}\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

// Read retained package bytes without following the saved junction back into
// the current runtime. The real rollback command must independently validate them.
fn retained_entry(root: &Path) -> PathBuf {
    fn collect(root: &Path, entries: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_attributes() & 0x400 != 0 {
                continue;
            }
            if metadata.is_dir() {
                collect(&path, entries);
            } else if path.file_name().unwrap() == "openclaw.mjs" {
                entries.push(path);
            }
        }
    }
    let mut entries = Vec::new();
    collect(root, &mut entries);
    assert_eq!(
        entries.len(),
        1,
        "expected one host-only physical entry: {entries:?}"
    );
    entries.pop().unwrap()
}

#[test]
fn native_windows_build_local_publishes_hoisted_and_linked_host_packages() {
    for strategy in ["hoisted", "linked"] {
        let fixture = NativeFixture::new("native-publication & %PATH% !literal! 雪", strategy);
        let source = fixture.source(OLD);
        let archive = fixture.pack(&source);
        let prefix = fixture.root.child("native-prefix");
        fs::create_dir_all(&prefix).unwrap();
        fixture.npm(
            fixture.root.path(),
            &[
                "install",
                "--prefix",
                &path_string(&prefix),
                "--omit=dev",
                "--no-save",
                "--package-lock=false",
                &format!("file:{}", archive.display()),
            ],
        );
        assert_eq!(
            fixture
                .node(&[
                    &path_string(&prefix.join("node_modules/openclaw/openclaw.mjs")),
                    "--version"
                ])
                .trim(),
            OLD
        );

        fixture.ok(&[
            "runtime",
            "build-local",
            "host",
            "--repo",
            &path_string(&source.root),
            "--json",
        ]);
        let metadata = fixture.assert_runtime("host", OLD, strategy == "linked");
        let before = fixture.metadata_bytes("host");
        fixture.bind("demo", "host", OLD);
        let binary = Path::new(metadata["binaryPath"].as_str().unwrap());
        let original = fs::read(binary).unwrap();
        fs::write(binary, b"throw new Error('corrupted package');\n").unwrap();
        let refused = fixture.ocm(&["runtime", "verify", "host", "--json"]);
        assert!(!refused.status.success());
        let unhealthy: Value = serde_json::from_str(&stdout(&refused)).unwrap();
        assert_eq!(unhealthy["healthy"], false);
        assert_eq!(fixture.metadata_bytes("host"), before);
        fs::write(binary, original).unwrap();
        fixture.assert_runtime("host", OLD, strategy == "linked");
        fixture.assert_binding("demo", "host", OLD);
        source.assert_unchanged();
    }
}

#[test]
fn native_windows_official_install_and_update_publish_linked_host_packages() {
    let mut fixture = NativeFixture::new("native-official-publication", "linked");
    let old = fixture.release(OLD);
    let new = fixture.release(NEW);
    let _registry = fixture.registry(&old, &new);
    fixture.ok(&[
        "runtime",
        "install",
        "stable",
        "--channel",
        "stable",
        "--json",
    ]);
    fixture.assert_runtime("stable", OLD, true);
    fixture.ok(&["runtime", "update", "stable", "--json"]);
    let metadata = fixture.assert_runtime("stable", NEW, true);
    assert_eq!(metadata["releaseVersion"], NEW);
    fixture.bind("demo", "stable", NEW);
    assert!(!old.archive.requests().is_empty());
    assert!(!new.archive.requests().is_empty());
    old.source.assert_unchanged();
    new.source.assert_unchanged();
}

#[test]
fn native_windows_in_place_upgrade_validates_retained_linked_bytes_and_rolls_back() {
    let mut fixture = NativeFixture::new("native-upgrade-retained-junction", "linked");
    let old = fixture.release(OLD);
    let new = fixture.release(NEW);
    let _registry = fixture.registry(&old, &new);
    fixture.ok(&[
        "runtime",
        "install",
        "stable",
        "--channel",
        "stable",
        "--json",
    ]);
    let old_metadata = fixture.metadata_bytes("stable");
    let old_runtime = fixture.assert_runtime("stable", OLD, true);
    let package_relative = "files/node_modules/openclaw";
    let old_target = fs::read_link(
        Path::new(old_runtime["installRoot"].as_str().unwrap()).join(package_relative),
    )
    .unwrap();
    fixture.bind("demo", "stable", OLD);
    let marker = fixture
        .root
        .child("ocm-home/envs/demo/.openclaw/workspace/publication-marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "before-upgrade").unwrap();

    let upgraded = fixture.json(&["upgrade", "demo", "--json"]);
    assert_eq!(upgraded["outcome"], "updated", "{upgraded}");
    fixture.assert_runtime("stable", NEW, true);
    fixture.assert_binding("demo", "stable", NEW);
    fs::write(&marker, "after-upgrade").unwrap();
    let history = fixture.json(&["upgrade", "history", "demo", "--json"]);
    assert_eq!(history.as_array().unwrap().len(), 1, "{history}");
    let transaction = history[0]["id"].as_str().unwrap();
    let recovery = fixture.root.child(format!(
        "ocm-home/upgrade-history/demo/{transaction}.recovery/stable"
    ));
    let retained = recovery.join("install-root");
    let retained_metadata = fs::read(recovery.join("runtime.json")).unwrap();
    assert_eq!(
        retained_metadata, old_metadata,
        "retention rewrote metadata"
    );
    assert_eq!(
        fs::read_link(retained.join(package_relative)).unwrap(),
        old_target
    );
    assert!(
        !retained
            .join(package_relative)
            .join("openclaw.mjs")
            .exists(),
        "saved junction unexpectedly resolves through the replacement's tree"
    );
    let saved_entry = retained_entry(&retained);
    assert_eq!(fs::read(&saved_entry).unwrap(), old.source.entry);
    assert_ne!(old.source.entry, new.source.entry);
    let current = fixture.json(&["runtime", "show", "stable", "--json"]);
    assert_eq!(
        fs::read(current["binaryPath"].as_str().unwrap()).unwrap(),
        new.source.entry
    );

    // A healthy replacement must never hide corruption in the saved old bytes.
    let current_metadata = fixture.metadata_bytes("stable");
    fs::write(&saved_entry, b"corrupted retained package\n").unwrap();
    let refused = fixture.ocm(&["upgrade", "rollback", "demo", "--json"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("runtime recovery"),
        "{}\n{}",
        stdout(&refused),
        stderr(&refused)
    );
    assert_eq!(fixture.metadata_bytes("stable"), current_metadata);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "after-upgrade");
    assert_eq!(
        fs::read(recovery.join("runtime.json")).unwrap(),
        retained_metadata
    );
    fixture.assert_binding("demo", "stable", NEW);
    fs::write(&saved_entry, &old.source.entry).unwrap();

    let rollback = fixture.json(&["upgrade", "rollback", "demo", "--json"]);
    assert_eq!(rollback["outcome"], "rolled-back", "{rollback}");
    fixture.assert_runtime("stable", OLD, true);
    fixture.assert_binding("demo", "stable", OLD);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "before-upgrade");
    assert!(
        !recovery.exists(),
        "retained recovery remains after rollback: {rollback}"
    );
    let reverse = fixture.json(&["upgrade", "rollback", "demo", "--json"]);
    assert_eq!(reverse["outcome"], "rolled-back", "{reverse}");
    fixture.assert_runtime("stable", NEW, true);
    fixture.assert_binding("demo", "stable", NEW);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "after-upgrade");
    old.source.assert_unchanged();
    new.source.assert_unchanged();
}

#[test]
fn native_windows_failed_finalizer_restores_linked_runtime_automatically() {
    let mut fixture = NativeFixture::new("native-upgrade-automatic-rollback", "linked");
    let old = fixture.release(OLD);
    let new = fixture.release(NEW);
    let _registry = fixture.registry(&old, &new);
    fixture.ok(&[
        "runtime",
        "install",
        "stable",
        "--channel",
        "stable",
        "--json",
    ]);
    let before = fixture.metadata_bytes("stable");
    fixture.bind("demo", "stable", OLD);
    let marker = fixture
        .root
        .child("ocm-home/envs/demo/.openclaw/workspace/publication-marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "preserved-state").unwrap();
    fixture
        .env
        .insert("OCM_TEST_NATIVE_FAIL_FINALIZE".into(), NEW.into());
    let upgrade = fixture.ocm(&["upgrade", "demo", "--json"]);
    assert!(!upgrade.status.success());
    let result: Value = serde_json::from_str(&stdout(&upgrade)).unwrap();
    assert_eq!(result["outcome"], "rolled-back", "{result}");
    assert!(
        stdout(&upgrade).contains("native fixture finalizer failure"),
        "{}\n{}",
        stdout(&upgrade),
        stderr(&upgrade)
    );
    assert_eq!(fixture.metadata_bytes("stable"), before);
    fixture.assert_runtime("stable", OLD, true);
    fixture.assert_binding("demo", "stable", OLD);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "preserved-state");
    let commands = fs::read_to_string(fixture.root.child("commands.jsonl")).unwrap();
    let finalizer = commands
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|entry| {
            entry["version"] == NEW
                && entry["args"][0] == "update"
                && entry["args"][1] == "finalize"
        })
        .expect("new package must execute before automatic rollback");
    assert!(
        !finalizer["entry"].as_str().unwrap().contains(".stage-"),
        "{finalizer}"
    );
    old.source.assert_unchanged();
    new.source.assert_unchanged();
}
