#![cfg(unix)]

mod support;

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Command;

use support::{TestDir, path_string, stderr, stdout};

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[test]
fn bin_wrapper_runs_with_an_overridden_home() {
    let root = TestDir::new("bin-wrapper-home-override");
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wrapper = repo_root.join("bin/ocm");
    let home = root.child("isolated-home");
    let ocm_home = root.child("ocm-home");
    let shim_bin = root.child("shim-bin");

    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ocm_home).unwrap();
    std::fs::create_dir_all(&shim_bin).unwrap();

    // Homebrew's rustup shell wrapper preserves its own command name, so
    // renaming it to cargo does not select Cargo proxy dispatch.
    let cargo = find_executable("cargo")
        .expect("cargo must be available for wrapper tests")
        .canonicalize()
        .expect("cargo must resolve to an installed entrypoint");
    // Native rustup installs cargo as a hard link too. Keep its rustup name
    // discoverable by the wrapper without substituting a different shell entrypoint.
    let rustup = cargo.with_file_name("rustup");
    let cargo = match (std::fs::metadata(&cargo), std::fs::metadata(&rustup)) {
        (Ok(cargo_meta), Ok(rustup_meta))
            if cargo_meta.dev() == rustup_meta.dev() && cargo_meta.ino() == rustup_meta.ino() =>
        {
            rustup
        }
        _ => cargo,
    };
    std::os::unix::fs::symlink(cargo, shim_bin.join("cargo")).unwrap();
    let path = std::env::join_paths(std::iter::once(shim_bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();

    let mut command = Command::new(&wrapper);
    command.current_dir(&repo_root);
    command.arg("help");
    command.env_clear();
    command.env("HOME", path_string(&home));
    command.env("OCM_HOME", path_string(&ocm_home));
    command.env("PATH", path);
    command.env_remove("RUSTUP_HOME");

    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "wrapper exited {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("OpenClaw Manager"));
}
