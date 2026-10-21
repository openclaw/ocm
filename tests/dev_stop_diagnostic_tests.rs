mod support;

use std::process::Command;

use support::{TestDir, ocm_env};

#[test]
fn native_controller_diagnostic_capture_excludes_session_contents() {
    let root = TestDir::new("dev-stop-diagnostic");
    let output = Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/dev_stop_diagnostic.cjs"
        ))
        .current_dir(root.path())
        .env_clear()
        .envs(ocm_env(&root))
        .output()
        .expect("run the native fixture's diagnostic decoder");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("privacy negatives"));
}
