mod support;

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use fs2::FileExt;
use ocm::store::{clean_path, env_registry_path};
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, install_fake_node_and_npm, ocm_env, run_ocm, stderr, stdout,
    write_executable_script,
};

fn append_tar_file(
    builder: &mut Builder<&mut GzEncoder<Vec<u8>>>,
    path: &str,
    body: &[u8],
    mode: u32,
) {
    let mut header = Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    builder.append_data(&mut header, path, body).unwrap();
}

fn openclaw_package_tarball(script_body: &str, version: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        append_tar_file(
            &mut builder,
            "package/openclaw.mjs",
            script_body.as_bytes(),
            0o755,
        );
        append_tar_file(
            &mut builder,
            "package/package.json",
            format!(
                "{{\"name\":\"openclaw\",\"version\":\"{version}\",\"bin\":{{\"openclaw\":\"openclaw.mjs\"}}}}"
            )
            .as_bytes(),
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn sha512_integrity(body: &[u8]) -> String {
    let digest = Sha512::digest(body);
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

#[test]
fn env_set_runtime_updates_and_clears_the_default_runtime() {
    let root = TestDir::new("runtime-binding-set");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let bind = run_ocm(&cwd, &env, &["env", "set-runtime", "demo", "stable"]);
    assert!(bind.status.success(), "{}", stderr(&bind));
    assert!(stdout(&bind).contains("Updated env demo: defaultRuntime=stable"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("defaultRuntime: stable"));

    let clear = run_ocm(&cwd, &env, &["env", "set-runtime", "demo", "none"]);
    assert!(clear.status.success(), "{}", stderr(&clear));
    assert!(stdout(&clear).contains("Updated env demo: defaultRuntime=none"));

    let show_cleared = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(show_cleared.status.success(), "{}", stderr(&show_cleared));
    assert!(!stdout(&show_cleared).contains("defaultRuntime:"));
}

#[test]
fn managed_backing_lock_refuses_alias_binding_until_release() {
    let root = TestDir::new("runtime-alias-binding-lock");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let binary = cwd.join("openclaw");
    write_executable_script(&binary, "#!/bin/sh\nprintf '2026.7.1\\n'\n");
    let env = ocm_env(&root);
    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "backing",
            "--path",
            binary.to_str().unwrap(),
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    let backing = ocm::store::get_runtime("backing", &env, &cwd).unwrap();
    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "selected", "--path", &backing.binary_path],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let existing = run_ocm(&cwd, &env, &["env", "create", "existing"]);
    assert!(existing.status.success(), "{}", stderr(&existing));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.child("ocm-home/runtimes/.backing.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    for (name, runtime) in [("late-alias", "selected"), ("late-direct", "backing")] {
        let create = run_ocm(&cwd, &env, &["env", "create", name, "--runtime", runtime]);
        assert!(!create.status.success());
        assert!(
            stderr(&create).contains("busy with another operation"),
            "{}",
            stderr(&create)
        );
        assert!(!root.child(&format!("ocm-home/envs/{name}")).exists());
    }
    let bind = run_ocm(&cwd, &env, &["env", "set-runtime", "existing", "selected"]);
    assert!(!bind.status.success());
    assert!(stderr(&bind).contains("busy with another operation"));
    assert!(
        ocm::store::get_environment("existing", &env, &cwd)
            .unwrap()
            .default_runtime
            .is_none()
    );
    let peer = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "unrelated",
            "--path",
            binary.to_str().unwrap(),
        ],
    );
    assert!(peer.status.success(), "{}", stderr(&peer));
    let peer = run_ocm(
        &cwd,
        &env,
        &["env", "create", "peer", "--runtime", "unrelated"],
    );
    assert!(peer.status.success(), "{}", stderr(&peer));
    fs2::FileExt::unlock(&lock).unwrap();
    for (name, runtime) in [("late-alias", "selected"), ("late-direct", "backing")] {
        let create = run_ocm(&cwd, &env, &["env", "create", name, "--runtime", runtime]);
        assert!(create.status.success(), "{}", stderr(&create));
    }
}

#[cfg(unix)]
#[test]
fn raw_clone_and_import_preserve_cyclic_bindings_with_managed_custody() {
    for managed in [false, true] {
        let root = TestDir::new("runtime-cycle-raw-binding");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let native = cwd.join("openclaw");
        write_executable_script(&native, "#!/bin/sh\nprintf '2026.7.1\\n'\n");
        let env = ocm_env(&root);
        let ok = |args: &[&str]| {
            let output = run_ocm(&cwd, &env, args);
            assert!(output.status.success(), "{args:?}: {}", stderr(&output));
            output
        };
        let first = cwd.join("first");
        let second = if managed {
            ok(&[
                "runtime",
                "install",
                "backing",
                "--path",
                native.to_str().unwrap(),
            ]);
            std::path::PathBuf::from(
                ocm::store::get_runtime("backing", &env, &cwd)
                    .unwrap()
                    .binary_path,
            )
        } else {
            let second = cwd.join("second");
            fs::copy(&native, &second).unwrap();
            second
        };
        std::os::unix::fs::symlink(&second, &first).unwrap();
        ok(&[
            "runtime",
            "add",
            "selected",
            "--path",
            first.to_str().unwrap(),
        ]);
        ok(&["env", "create", "source", "--runtime", "selected"]);
        let marker = root.child("ocm-home/envs/source/durable-marker");
        fs::write(&marker, "preserve me").unwrap();
        ok(&["env", "clone", "source", "healthy"]);
        let before = stdout(&ok(&["env", "show", "source", "--json"]));
        let archive = cwd.join("source.ocm-env.tar");
        ok(&[
            "env",
            "export",
            "source",
            "--output",
            archive.to_str().unwrap(),
        ]);
        fs::remove_file(&second).unwrap();
        std::os::unix::fs::symlink(&first, &second).unwrap();
        if managed {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.child("ocm-home/runtimes/.backing.lock"))
                .unwrap();
            lock.lock_exclusive().unwrap();
            for args in [
                vec!["env", "clone", "source", "blocked"],
                vec![
                    "env",
                    "import",
                    archive.to_str().unwrap(),
                    "--name",
                    "blocked",
                ],
            ] {
                let output = run_ocm(&cwd, &env, &args);
                assert!(!output.status.success());
                assert!(
                    stderr(&output).contains("busy with another operation"),
                    "{}",
                    stderr(&output)
                );
                assert!(!root.child("ocm-home/envs/blocked").exists());
            }
            fs2::FileExt::unlock(&lock).unwrap();
        }
        ok(&["env", "clone", "source", "cyclic"]);
        ok(&[
            "env",
            "import",
            archive.to_str().unwrap(),
            "--name",
            "imported",
        ]);
        for name in ["cyclic", "imported"] {
            assert_eq!(
                ocm::store::get_environment(name, &env, &cwd)
                    .unwrap()
                    .default_runtime
                    .as_deref(),
                Some("selected")
            );
            assert_eq!(
                fs::read_to_string(root.child(&format!("ocm-home/envs/{name}/durable-marker")))
                    .unwrap(),
                "preserve me"
            );
        }
        let validated = run_ocm(
            &cwd,
            &env,
            &["env", "create", "validated", "--runtime", "selected"],
        );
        assert!(!validated.status.success());
        assert!(!root.child("ocm-home/envs/validated").exists());
        assert_eq!(stdout(&ok(&["env", "show", "source", "--json"])), before);
        assert_eq!(fs::read_to_string(&marker).unwrap(), "preserve me");
        ok(&[
            "runtime",
            "install",
            "unrelated",
            "--path",
            native.to_str().unwrap(),
        ]);
    }
}

#[test]
fn runtime_binding_and_removal_share_the_environment_registry_lock() {
    let root = TestDir::new("runtime-binding-remove-lock");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let lock_path = env_registry_path(&env, &cwd)
        .unwrap()
        .with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    lock.lock_exclusive().unwrap();

    let mut bind = Command::new(env!("CARGO_BIN_EXE_ocm"));
    bind.current_dir(&cwd)
        .args(["env", "set-runtime", "demo", "stable"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut bind = bind.spawn().unwrap();
    thread::sleep(Duration::from_millis(100));

    let mut remove = Command::new(env!("CARGO_BIN_EXE_ocm"));
    remove
        .current_dir(&cwd)
        .args(["runtime", "remove", "stable"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut remove = remove.spawn().unwrap();
    thread::sleep(Duration::from_millis(100));

    assert!(bind.try_wait().unwrap().is_none());
    assert!(remove.try_wait().unwrap().is_none());
    FileExt::unlock(&lock).unwrap();

    let bind = bind.wait_with_output().unwrap();
    let remove = remove.wait_with_output().unwrap();
    assert_ne!(bind.status.success(), remove.status.success());

    let environment = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(environment.status.success(), "{}", stderr(&environment));
    let environment: serde_json::Value = serde_json::from_str(&stdout(&environment)).unwrap();
    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    if bind.status.success() {
        assert!(runtime.status.success(), "{}", stderr(&runtime));
        assert_eq!(environment["defaultRuntime"], "stable");
    } else {
        assert!(!runtime.status.success());
        assert!(environment["defaultRuntime"].is_null());
    }
}

#[test]
fn env_set_runtime_clears_an_existing_launcher_binding() {
    let root = TestDir::new("runtime-binding-clears-launcher");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));

    let add_launcher = run_ocm(&cwd, &env, &["launcher", "add", "dev", "--command", "sh"]);
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let bind = run_ocm(&cwd, &env, &["env", "set-runtime", "demo", "stable"]);
    assert!(bind.status.success(), "{}", stderr(&bind));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("defaultRuntime: stable"));
    assert!(!output.contains("defaultLauncher:"));
}

#[test]
fn env_set_runtime_with_channel_installs_and_binds_the_official_runtime() {
    let root = TestDir::new("runtime-binding-set-channel");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/bin/sh\nprintf 'official-rebind'\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let bind = run_ocm(
        &cwd,
        &env,
        &["env", "set-runtime", "demo", "--channel", "stable"],
    );
    assert!(bind.status.success(), "{}", stderr(&bind));
    assert!(stdout(&bind).contains("Updated env demo: defaultRuntime=stable"));

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_stdout = stdout(&runtime);
    assert!(runtime_stdout.contains("\"releaseSelectorKind\": \"channel\""));
    assert!(runtime_stdout.contains("\"releaseSelectorValue\": \"stable\""));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "official-rebind");
}

#[test]
fn env_run_uses_the_registered_runtime_binary() {
    let root = TestDir::new("runtime-binding-run");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("stable"),
        "#!/bin/sh\nprintf 'runtime|%s|%s' \"$OPENCLAW_HOME\" \"$PWD\"\n",
    );
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--"]);
    assert!(run.status.success(), "{}", stderr(&run));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    let expected_cwd = fs::canonicalize(&cwd).unwrap();
    assert_eq!(
        stdout(&run),
        format!("runtime|{}|{}", env_root.display(), expected_cwd.display())
    );
}

#[test]
fn env_run_prefers_the_bound_runtime_over_the_bound_launcher() {
    let root = TestDir::new("runtime-binding-precedence");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nprintf 'runtime'\n");
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "fallback",
            "--command",
            "printf launcher",
        ],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "demo",
            "--runtime",
            "stable",
            "--launcher",
            "fallback",
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "runtime");
}

#[test]
fn env_run_uses_a_runtime_installed_from_url() {
    let root = TestDir::new("runtime-binding-installed-url");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let server = TestHttpServer::serve_bytes(
        "/releases/openclaw-nightly",
        "application/octet-stream",
        b"#!/bin/sh\nprintf 'runtime-url|%s|%s' \"$OPENCLAW_HOME\" \"$PWD\"\n",
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "nightly", "--url", &server.url()],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "nightly"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--"]);
    assert!(run.status.success(), "{}", stderr(&run));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    let expected_cwd = fs::canonicalize(&cwd).unwrap();
    assert_eq!(
        stdout(&run),
        format!(
            "runtime-url|{}|{}",
            env_root.display(),
            expected_cwd.display()
        )
    );
}

#[test]
fn env_create_with_channel_installs_and_binds_the_official_runtime() {
    let root = TestDir::new("runtime-binding-create-channel");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball(
        "#!/bin/sh\nprintf 'official-stable|%s|%s' \"$OPENCLAW_HOME\" \"$PWD\"\n",
        "2026.3.24",
    );
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--channel", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("\"defaultRuntime\": \"stable\""));

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_stdout = stdout(&runtime);
    assert!(runtime_stdout.contains("\"releaseVersion\": \"2026.3.24\""));
    assert!(runtime_stdout.contains("\"releaseChannel\": \"stable\""));
    assert!(runtime_stdout.contains("\"releaseSelectorKind\": \"channel\""));
    assert!(runtime_stdout.contains("\"releaseSelectorValue\": \"stable\""));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--"]);
    assert!(run.status.success(), "{}", stderr(&run));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    let expected_cwd = fs::canonicalize(&cwd).unwrap();
    assert_eq!(
        stdout(&run),
        format!(
            "official-stable|{}|{}",
            env_root.display(),
            expected_cwd.display()
        )
    );
}

#[test]
fn env_create_with_latest_channel_alias_binds_the_stable_runtime_name() {
    let root = TestDir::new("runtime-binding-create-latest");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/bin/sh\nprintf 'official-latest'\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--channel", "latest"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("\"defaultRuntime\": \"stable\""));

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_stdout = stdout(&runtime);
    assert!(runtime_stdout.contains("\"releaseSelectorKind\": \"channel\""));
    assert!(runtime_stdout.contains("\"releaseSelectorValue\": \"stable\""));
}

#[test]
fn env_create_with_version_installs_and_binds_the_official_runtime() {
    let root = TestDir::new("runtime-binding-create-version");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/bin/sh\nprintf 'official-version'\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--version", "2026.3.24"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("\"defaultRuntime\": \"2026.3.24\""));

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "2026.3.24", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_stdout = stdout(&runtime);
    assert!(runtime_stdout.contains("\"releaseVersion\": \"2026.3.24\""));
    assert!(runtime_stdout.contains("\"releaseSelectorKind\": \"version\""));
    assert!(runtime_stdout.contains("\"releaseSelectorValue\": \"2026.3.24\""));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "official-version");
}

#[test]
fn env_create_rejects_conflicting_runtime_and_release_selector_flags() {
    let root = TestDir::new("runtime-binding-create-conflict-runtime-selector");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "demo",
            "--runtime",
            "stable",
            "--channel",
            "stable",
        ],
    );
    assert!(!create.status.success());
    assert!(stderr(&create).contains(
        "env create accepts only one runtime source: --runtime, --version, or --channel"
    ));
}

#[test]
fn env_create_rejects_conflicting_version_and_channel_flags() {
    let root = TestDir::new("runtime-binding-create-conflict-version-channel");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "demo",
            "--version",
            "2026.3.24",
            "--channel",
            "stable",
        ],
    );
    assert!(!create.status.success());
    assert!(stderr(&create).contains("env create accepts only one of --version or --channel"));
}

#[test]
fn env_set_runtime_rejects_conflicting_runtime_and_release_selector_flags() {
    let root = TestDir::new("runtime-binding-set-conflict-runtime-selector");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let bind = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "set-runtime",
            "demo",
            "stable",
            "--channel",
            "stable",
        ],
    );
    assert!(!bind.status.success());
    assert!(stderr(&bind).contains(
        "env set-runtime accepts only one runtime source: --runtime, --version, or --channel"
    ));
}

#[test]
fn env_set_runtime_rejects_conflicting_version_and_channel_flags() {
    let root = TestDir::new("runtime-binding-set-conflict-version-channel");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let bind = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "set-runtime",
            "demo",
            "--version",
            "2026.3.24",
            "--channel",
            "stable",
        ],
    );
    assert!(!bind.status.success());
    assert!(stderr(&bind).contains("env set-runtime accepts only one of --version or --channel"));
}
