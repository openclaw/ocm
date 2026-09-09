#![cfg(windows)]

mod support;

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::support::{TestDir, ocm_env, ocm_test_binary_path};

#[test]
fn native_windows_dev_stop_lifecycle() {
    let fixture = TestDir::new("native-windows-dev-stop");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let helper = repo.join("tests/support/windows_dev_stop.cjs");
    let mut command = Command::new("node");
    command
        // The helper removes its fixture root, so Node must run outside it.
        .current_dir(repo)
        .arg(helper)
        .arg(ocm_test_binary_path())
        .arg(fixture.path())
        .env_clear()
        .envs(ocm_env(&fixture));
    for key in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
        "PROCESSOR_ARCHITECTURE",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = command
        .output()
        .expect("run the native Windows dev-stop helper");
    assert!(
        output.status.success(),
        "native Windows dev-stop helper failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid native Windows dev-stop result: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(result["platform"], "win32");
    assert_eq!(
        result["results"].as_array().map(Vec::len),
        Some(4),
        "not all native Windows dev-stop cases completed: {result}"
    );
    assert_eq!(result["passed"], true);
    assert_eq!(result["fixtureRemoved"], true);
    assert!(
        !fixture.path().exists(),
        "native Windows helper left its private fixture behind"
    );
}
