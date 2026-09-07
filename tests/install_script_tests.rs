#![cfg(unix)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use crate::support::{TestDir, path_string, stderr, write_executable_script};

fn installer_requested_urls(os: &str, arch: &str, version: Option<&str>) -> Vec<String> {
    let root = TestDir::new("install-release-urls");
    let fake_bin = root.child("bin");
    let url_log = root.child("urls");
    fs::create_dir_all(&fake_bin).unwrap();
    write_executable_script(
        &fake_bin.join("uname"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  -s) printf '{os}\\n' ;;\n  -m) printf '{arch}\\n' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    );
    write_executable_script(
        &fake_bin.join("curl"),
        r#"#!/bin/sh
set -eu
url=""
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift; output="$1" ;;
    http*) url="$1" ;;
  esac
  shift
done
printf '%s\n' "$url" >>"$TEST_URL_LOG"
: >"$output"
"#,
    );

    let system_path = std::env::var_os("PATH").unwrap_or_default();
    let mut command = Command::new("bash");
    command
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
        .args(["--bin-dir", &path_string(&root.child("installed"))])
        .env("HOME", root.child("home"))
        .env("TEST_URL_LOG", &url_log)
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), system_path.to_string_lossy()),
        );
    if let Some(version) = version {
        command.args(["--version", version]);
    }

    let output = command.output().unwrap();
    assert!(!output.status.success());
    fs::read_to_string(url_log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn installer_uses_canonical_latest_release_urls() {
    assert_eq!(
        installer_requested_urls("Darwin", "x86_64", None),
        [
            "https://github.com/openclaw/ocm/releases/latest/download/ocm-x86_64-apple-darwin.tar.gz",
            "https://github.com/openclaw/ocm/releases/latest/download/SHA256SUMS",
        ]
    );
}

#[test]
fn installer_uses_canonical_versioned_release_urls() {
    assert_eq!(
        installer_requested_urls("Darwin", "x86_64", Some("v0.2.33")),
        [
            "https://github.com/openclaw/ocm/releases/download/v0.2.33/ocm-x86_64-apple-darwin.tar.gz",
            "https://github.com/openclaw/ocm/releases/download/v0.2.33/SHA256SUMS",
        ]
    );
}

#[test]
fn installer_selects_every_supported_release_target() {
    for (os, arch, target) in [
        ("Darwin", "arm64", "aarch64-apple-darwin"),
        ("Darwin", "x86_64", "x86_64-apple-darwin"),
        ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
    ] {
        let urls = installer_requested_urls(os, arch, Some("v0.2.33"));
        assert_eq!(
            urls[0],
            format!(
                "https://github.com/openclaw/ocm/releases/download/v0.2.33/ocm-{target}.tar.gz"
            )
        );
        assert_eq!(
            urls[1],
            "https://github.com/openclaw/ocm/releases/download/v0.2.33/SHA256SUMS"
        );
    }
}

#[test]
fn installer_rejects_linux_aarch64_before_downloading() {
    let root = TestDir::new("install-linux-aarch64");
    let bin_dir = root.child("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let uname = bin_dir.join("uname");
    fs::write(
        &uname,
        "#!/bin/sh\ncase \"$1\" in\n  -s) printf 'Linux\\n' ;;\n  -m) printf 'aarch64\\n' ;;\n  *) exit 1 ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&uname, fs::Permissions::from_mode(0o755)).unwrap();

    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut command = Command::new("bash");
    command
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
        .env("HOME", root.child("home"))
        .env(
            "PATH",
            format!("{}:{}", bin_dir.display(), path.to_string_lossy()),
        );
    let output = command.output().unwrap();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("unsupported platform: aarch64-unknown-linux-gnu"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn installer_force_preserves_managed_destinations_before_download() {
    let root = TestDir::new("install-managed-destination");
    let fake_bin = root.child("fake-bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let log = root.child("curl.log");
    write_executable_script(
        &fake_bin.join("curl"),
        "#!/bin/sh\nprintf 'called\\n' >> \"$TEST_CURL_LOG\"\nexit 1\n",
    );
    let native_dir = root.child(
        "prefix/lib/node_modules/@openclaw/ocm-linux-x64/vendor/x86_64-unknown-linux-gnu/bin",
    );
    let brew_dir = root.child("Cellar/ocm/0.2.39/bin");
    let linked_dir = root.child("bin");
    let dangling_dir = root.child("dangling");
    for directory in [&native_dir, &brew_dir, &linked_dir, &dangling_dir] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(native_dir.join("ocm"), "npm-owned").unwrap();
    fs::write(brew_dir.join("ocm"), "brew-owned").unwrap();
    fs::write(brew_dir.join("../INSTALL_RECEIPT.json"), "{}").unwrap();
    std::os::unix::fs::symlink(native_dir.join("ocm"), linked_dir.join("ocm")).unwrap();
    std::os::unix::fs::symlink(root.child("missing"), dangling_dir.join("ocm")).unwrap();
    let missing = root.child("project/node_modules/alias/vendor/target/bin");
    fs::create_dir_all(root.child("project/node_modules")).unwrap();
    std::os::unix::fs::symlink(root.child("project/node_modules"), root.child("packages")).unwrap();
    let linked_missing = root.child("packages/alias/vendor/target/bin");
    for directory in [
        &native_dir,
        &brew_dir,
        &linked_dir,
        &dangling_dir,
        &missing,
        &linked_missing,
    ] {
        let output = Command::new("bash")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
            .args(["--force", "--bin-dir", &path_string(directory)])
            .env("HOME", root.child("home"))
            .env("TEST_CURL_LOG", &log)
            .env(
                "PATH",
                format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            !log.exists(),
            "installer downloaded before protecting {}",
            directory.display()
        );
    }
    assert_eq!(
        fs::read_to_string(native_dir.join("ocm")).unwrap(),
        "npm-owned"
    );
    assert_eq!(
        fs::read_to_string(brew_dir.join("ocm")).unwrap(),
        "brew-owned"
    );
    assert!(linked_dir.join("ocm").is_symlink());
    assert!(dangling_dir.join("ocm").is_symlink());
    assert!(!missing.exists());
    assert!(!linked_missing.exists());
}

#[test]
fn installer_preserves_destination_changes_and_standalone_force() {
    use flate2::{Compression, write::GzEncoder};
    for action in ["force", "new-file", "new-symlink", "managed-parent"] {
        let root = TestDir::new("install-force-standalone");
        let fake_bin = root.child("fake-bin");
        fs::create_dir_all(&fake_bin).unwrap();
        let archive = root.child("archive.tar.gz");
        let mut builder = tar::Builder::new(GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            Compression::default(),
        ));
        let bytes = b"new standalone binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, "ocm", &bytes[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let digest = ocm::infra::download::file_sha256(&archive).unwrap();
        write_executable_script(
            &fake_bin.join("uname"),
            "#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -m) echo x86_64;; esac\n",
        );
        write_executable_script(
            &fake_bin.join("curl"),
            &format!(
                r#"#!/bin/sh
case "$2" in
  */SHA256SUMS)
    printf '{digest}  ocm-x86_64-unknown-linux-gnu.tar.gz\n' > "$4"
    case "$TEST_ACTION" in
      new-file) printf 'appeared' > "$TEST_DESTINATION";;
      new-symlink) ln -s "$TEST_MANAGED_DEST" "$TEST_DESTINATION";;
      managed-parent)
        mv "$TEST_BIN_DIR" "$TEST_BIN_DIR.original"
        ln -s "$(dirname "$TEST_MANAGED_DEST")" "$TEST_BIN_DIR"
        ;;
    esac
    ;;
  *) cp "$TEST_ARCHIVE" "$4";;
esac
"#
            ),
        );
        let destination = root.child("standalone/ocm");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let managed = root.child("node_modules/@openclaw/ocm/bin/ocm");
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::write(&managed, "managed").unwrap();
        let mut command = Command::new("bash");
        if action == "force" {
            fs::write(&destination, "old").unwrap();
            command
                .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
                .arg("--force");
        } else {
            command.arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh"));
        }
        let output = command
            .args(["--bin-dir", &path_string(destination.parent().unwrap())])
            .env("HOME", root.child("home"))
            .env("TEST_ARCHIVE", archive)
            .env("TEST_ACTION", action)
            .env("TEST_DESTINATION", &destination)
            .env("TEST_BIN_DIR", destination.parent().unwrap())
            .env("TEST_MANAGED_DEST", &managed)
            .env(
                "PATH",
                format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        if action == "force" {
            assert!(output.status.success(), "{}", stderr(&output));
            assert_eq!(fs::read(&destination).unwrap(), bytes);
        } else {
            assert!(!output.status.success(), "{action}");
            assert_eq!(
                fs::read(&destination).unwrap(),
                if action == "new-file" {
                    b"appeared".as_slice()
                } else {
                    b"managed".as_slice()
                }
            );
        }
        assert_eq!(fs::read(managed).unwrap(), b"managed");
    }
}
