mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Output;

use support::{TestDir, ocm_env, run_ocm, stderr, stdout};

struct Fixture {
    temp: TestDir,
    env: BTreeMap<String, String>,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TestDir::new("env-artifact");
        let env = ocm_env(&temp);
        let create = run_ocm(temp.path(), &env, &["env", "create", "demo"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let home = temp.child("ocm-home/envs/demo");
        Self { temp, env, home }
    }

    fn export(&self, path: &str, bound: &str) -> Output {
        run_ocm(
            self.temp.path(),
            &self.env,
            &[
                "env",
                "artifact",
                "export",
                "demo",
                "--path",
                path,
                "--max-bytes",
                bound,
            ],
        )
    }

    fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
}

#[cfg(unix)]
#[test]
fn artifact_export_preserves_raw_bytes_at_the_exact_limit() {
    let fixture = Fixture::new();
    let relative = "reports/a 'quoted' $(literal) `name`.bin";
    let bytes = b"\0\xffraw\r\n::warning::literal\n";
    let file = fixture.write(relative, bytes);
    let registry = fixture.temp.child("ocm-home/envs.json");
    let registry_before = fs::read(&registry).unwrap();

    let output = fixture.export(relative, &bytes.len().to_string());

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output.stdout, bytes);
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
    assert_eq!(fs::read(file).unwrap(), bytes);
    assert_eq!(fs::read(registry).unwrap(), registry_before);

    fixture.write("empty", b"");
    let empty = fixture.export("empty", "0");
    assert!(empty.status.success(), "{}", stderr(&empty));
    assert!(empty.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn artifact_export_reads_the_home_for_owned_and_borrowed_sources() {
    use ocm::env::{EnvDevMeta, EnvironmentService};
    use ocm::store::{env_registry_path, save_environment};
    use std::path::Path;
    use std::process::Command;
    use support::{path_string, write_text};

    for (label, borrowed, linked) in [
        ("owned", false, true),
        ("borrowed-main", true, false),
        ("borrowed-linked", true, true),
    ] {
        let fixture = Fixture::new();
        let repo = fixture.temp.child("repo");
        write_text(&repo.join("package.json"), r#"{"name":"openclaw"}"#);
        write_text(&repo.join("scripts/run-node.mjs"), "// retained source\n");
        write_text(&repo.join("reports/result"), "source bytes\n");
        write_text(&repo.join("source-only"), "not an environment artifact\n");
        let git = |path: &Path, args: &[&str]| {
            let output = Command::new("git")
                .current_dir(path)
                .args(args)
                .env_clear()
                .envs(&fixture.env)
                .output()
                .unwrap();
            assert!(output.status.success(), "{label}: {}", stderr(&output));
            output.stdout
        };
        git(&repo, &["init"]);
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=OCM Tests",
                "-c",
                "user.email=tests@example.com",
                "commit",
                "-m",
                "fixture",
            ],
        );
        let source = if linked {
            git(&repo, &["worktree", "add", "--detach", "../linked"]);
            fixture.temp.child("linked")
        } else {
            repo.clone()
        };
        let source = fs::canonicalize(source).unwrap();
        let service = EnvironmentService::new(&fixture.env, fixture.temp.path());
        let mut meta = service.get("demo").unwrap();
        meta.dev = Some(if borrowed {
            EnvDevMeta::Borrowed {
                source_root: path_string(&source),
            }
        } else {
            EnvDevMeta::Owned {
                repo_root: path_string(&fs::canonicalize(&repo).unwrap()),
                worktree_root: path_string(&source),
            }
        });
        save_environment(meta, &fixture.env, fixture.temp.path()).unwrap();
        let registry = env_registry_path(&fixture.env, fixture.temp.path()).unwrap();
        let registry_before = fs::read(&registry).unwrap();
        let source_status = git(&source, &["status", "--porcelain"]);
        fixture.write("reports/result", b"home bytes\n");

        let output = fixture.export("reports/result", "1024");
        assert!(output.status.success(), "{label}: {}", stderr(&output));
        assert_eq!(output.stdout, b"home bytes\n", "{label}");
        assert!(output.stderr.is_empty(), "{label}: {}", stderr(&output));
        let missing = fixture.export("source-only", "1024");
        assert_eq!(missing.status.code(), Some(1), "{label}");
        assert!(missing.stdout.is_empty(), "{label}");
        assert!(stderr(&missing).contains("open artifact"), "{label}");
        assert_eq!(git(&source, &["status", "--porcelain"]), source_status);

        let retained_source = if borrowed {
            let moved = fixture.temp.child("moved-source");
            fs::rename(&source, &moved).unwrap();
            let output = fixture.export("reports/result", "1024");
            assert!(output.status.success(), "{label}: {}", stderr(&output));
            assert_eq!(output.stdout, b"home bytes\n", "{label}");
            assert!(output.stderr.is_empty(), "{label}: {}", stderr(&output));
            assert!(!source.exists(), "{label}");
            moved
        } else {
            source
        };
        assert_eq!(
            fs::read(retained_source.join("reports/result")).unwrap(),
            b"source bytes\n",
            "{label}"
        );
        assert_eq!(
            fs::read(retained_source.join("source-only")).unwrap(),
            b"not an environment artifact\n",
            "{label}"
        );
        assert_eq!(fs::read(&registry).unwrap(), registry_before, "{label}");
    }
}

#[cfg(unix)]
#[test]
fn artifact_export_rejects_oversize_and_invalid_paths_without_output() {
    let fixture = Fixture::new();
    let file = fixture.write("report.json", b"1234");
    let absolute = file.to_str().unwrap();
    for (path, bound, reason) in [
        ("report.json", "3", "exceeds --max-bytes"),
        ("report.json", "0", "exceeds --max-bytes"),
        ("../report.json", "4", "relative path"),
        ("nested/../report.json", "4", "relative path"),
        (absolute, "4", "relative path"),
        ("", "4", "relative path"),
        (".", "4", "relative path"),
        ("missing", "4", "open artifact"),
    ] {
        let output = fixture.export(path, bound);
        assert_eq!(output.status.code(), Some(1), "{path}: {}", stderr(&output));
        assert!(output.stdout.is_empty(), "{path}");
        assert!(
            stderr(&output).contains(reason),
            "{path}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn artifact_export_requires_explicit_valid_arguments_and_has_help() {
    let fixture = Fixture::new();
    for bound in ["-1", "+1", "1.5", " 4", "18446744073709551616", ""] {
        let output = fixture.export("report", bound);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            stderr(&output).contains("--max-bytes"),
            "{}",
            stderr(&output)
        );
    }
    for args in [
        vec!["env", "artifact", "export", "demo", "--path", "report"],
        vec!["env", "artifact", "export", "demo", "--max-bytes", "4"],
        vec![
            "env",
            "artifact",
            "export",
            "demo",
            "--path",
            "report",
            "--max-bytes",
            "4",
            "--output",
            "retained",
        ],
    ] {
        let output = run_ocm(fixture.temp.path(), &fixture.env, &args);
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        assert!(output.stdout.is_empty());
    }
    let help = run_ocm(
        fixture.temp.path(),
        &fixture.env,
        &["env", "artifact", "export", "--help"],
    );
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("--max-bytes"));
    assert!(stdout(&help).contains("stdout"));
}

#[cfg(unix)]
#[test]
fn artifact_export_rejects_links_directories_and_special_files() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let fixture = Fixture::new();
    let outside = fixture.temp.child("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"outside bytes").unwrap();
    symlink(outside.join("secret"), fixture.home.join("leaf-link")).unwrap();
    symlink(&outside, fixture.home.join("dir-link")).unwrap();
    fs::hard_link(outside.join("secret"), fixture.home.join("hard-link")).unwrap();
    let local = fixture.write("local", b"local bytes");
    symlink(local, fixture.home.join("internal-link")).unwrap();
    fs::create_dir(fixture.home.join("directory")).unwrap();
    let fifo = CString::new(fixture.home.join("fifo").as_os_str().as_bytes()).unwrap();
    // O_NONBLOCK must keep an unwritten FIFO from hanging the export command.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    // Bind at a short path for macOS, then retain the real socket under the home.
    let socket_path = fixture.temp.child("s");
    let _socket = UnixListener::bind(&socket_path).unwrap();
    fs::rename(socket_path, fixture.home.join("socket")).unwrap();

    for relative in [
        "leaf-link",
        "dir-link/secret",
        "internal-link",
        "hard-link",
        "directory",
        "fifo",
        "socket",
    ] {
        let output = fixture.export(relative, "1024");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{relative}: {}",
            stderr(&output)
        );
        assert!(output.stdout.is_empty(), "{relative}");
    }

    let moved = fixture.temp.child("moved-home");
    fs::rename(&fixture.home, &moved).unwrap();
    symlink(&moved, &fixture.home).unwrap();
    let output = fixture.export("local", "1024");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside bytes");
}

#[cfg(not(unix))]
#[test]
fn artifact_export_fails_closed_on_unsupported_platforms() {
    let fixture = Fixture::new();
    fixture.write("report", b"bytes");
    let output = fixture.export("report", "5");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr(&output).contains("requires Unix"));
}

#[cfg(unix)]
#[test]
fn artifact_export_detects_source_changes_and_failed_output() {
    use ocm::env::EnvironmentService;
    use std::io::{self, Write};
    use std::path::Path;

    struct ChangingOutput<'a> {
        file: &'a Path,
        replace: bool,
        replacement: &'a [u8],
        bytes: Vec<u8>,
    }

    impl Write for ChangingOutput<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.replace {
                fs::rename(self.file, self.file.with_extension("old"))?;
            }
            fs::write(self.file, self.replacement)?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let fixture = Fixture::new();
    let service = EnvironmentService::new(&fixture.env, fixture.temp.path());
    for (replace, replacement) in [
        (false, b"short".as_slice()),
        (false, b"longer than original".as_slice()),
        (false, b"modified".as_slice()),
        (true, b"original".as_slice()),
    ] {
        let file = fixture.write("changing", b"original");
        let mut output = ChangingOutput {
            file: &file,
            replace,
            replacement,
            bytes: Vec::new(),
        };
        let error = service
            .export_artifact("demo", Path::new("changing"), 8, &mut output)
            .unwrap_err();
        assert!(error.contains("changed during export"), "{error}");
        assert!(output.bytes.len() <= 8);
    }

    fixture.write("report", b"bytes");
    let mut no_space = &mut [0_u8; 1][..];
    let error = service
        .export_artifact("demo", Path::new("report"), 5, &mut no_space)
        .unwrap_err();
    assert!(error.contains("write artifact"), "{error}");
}

#[cfg(unix)]
#[test]
fn artifact_export_fails_when_the_receiver_closes_stdout() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};

    let fixture = Fixture::new();
    fixture.write("report", b"bytes");
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let output = Command::new(support::ocm_test_binary_path())
        .args([
            "env",
            "artifact",
            "export",
            "demo",
            "--path",
            "report",
            "--max-bytes",
            "5",
        ])
        .current_dir(fixture.temp.path())
        .env_clear()
        .envs(&fixture.env)
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("failed to write artifact"),
        "{}",
        stderr(&output)
    );
}
