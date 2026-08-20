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
