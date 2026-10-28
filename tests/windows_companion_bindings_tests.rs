#![cfg(windows)]

mod support;

use std::fs;
use std::io::{self, Read, Write};
use std::mem::{offset_of, size_of};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::{Command, Stdio};
use std::ptr;
use std::time::{Duration, Instant};

use ocm::infra::download::file_sha256;
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_PRIVILEGE_NOT_HELD, GetLastError, LUID, SetLastError,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    SE_PRIVILEGE_REMOVED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenPrivileges,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use support::{TestDir, ocm_env, ocm_test_binary_path, path_string, windows_job::FixtureJob};

const CHILD_ROOT: &str = "OCM_TEST_COMPANION_PRIVILEGE_CHILD";
const TEST_NAME: &str = "native_windows_companion_without_symlink_privilege";

fn process_token(access: u32) -> io::Result<OwnedHandle> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

fn symlink_privilege() -> io::Result<LUID> {
    let name = "SeCreateSymbolicLinkPrivilege\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let mut luid = LUID::default();
    if unsafe { LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &mut luid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(luid)
}

fn has_privilege(token: &OwnedHandle, luid: &LUID) -> io::Result<bool> {
    let mut needed = 0;
    let result = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenPrivileges,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if result != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return Err(io::Error::other("could not size process token privileges"));
    }
    if !(size_of::<u32>()..=64 * 1024).contains(&(needed as usize)) {
        return Err(io::Error::other("invalid token privilege buffer size"));
    }
    // TOKEN_PRIVILEGES has a variable-length, naturally aligned trailing array.
    let mut storage = vec![0_usize; (needed as usize).div_ceil(size_of::<usize>())];
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenPrivileges,
            storage.as_mut_ptr().cast(),
            needed,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if returned < size_of::<u32>() as u32 || returned > needed {
        return Err(io::Error::other("invalid token privilege response size"));
    }
    let bytes = storage.as_ptr().cast::<u8>();
    let count = unsafe { bytes.cast::<u32>().read() } as usize;
    let offset = offset_of!(TOKEN_PRIVILEGES, Privileges);
    let end = count
        .checked_mul(size_of::<LUID_AND_ATTRIBUTES>())
        .and_then(|length| offset.checked_add(length))
        .ok_or_else(|| io::Error::other("token privilege count overflow"))?;
    if end > returned as usize {
        return Err(io::Error::other("truncated token privilege array"));
    }
    let privileges = unsafe {
        std::slice::from_raw_parts(bytes.add(offset).cast::<LUID_AND_ATTRIBUTES>(), count)
    };
    Ok(privileges
        .iter()
        .any(|entry| entry.Luid.LowPart == luid.LowPart && entry.Luid.HighPart == luid.HighPart))
}

fn remove_child_symlink_privilege(root: &Path) {
    let token = process_token(TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES).unwrap();
    let luid = symlink_privilege().unwrap();
    let present = has_privilege(&token, &luid).unwrap();
    if present {
        let removal = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_REMOVED,
            }],
        };
        unsafe { SetLastError(0) };
        let result = unsafe {
            AdjustTokenPrivileges(
                token.as_raw_handle(),
                0,
                &removal,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        let error = unsafe { GetLastError() };
        assert_ne!(result, 0, "remove symlink privilege: {error}");
        assert_eq!(error, 0, "symlink privilege was not removed: {error}");
    }
    assert!(!has_privilege(&token, &luid).unwrap());

    let probe = root.join("privilege-probe");
    fs::create_dir_all(probe.join("source")).unwrap();
    fs::create_dir_all(probe.join("links")).unwrap();
    fs::write(probe.join("links/writable"), "ordinary writes work").unwrap();
    let link = probe.join("links/link");
    let result = std::os::windows::fs::symlink_dir("../source", &link);
    if result.is_ok() {
        fs::remove_dir(&link).unwrap();
        panic!(
            "Windows prerequisite not met: std::symlink_dir succeeded without the token privilege; Developer Mode may permit it. No compatibility result was produced."
        );
    }
    let error = result.unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(ERROR_PRIVILEGE_NOT_HELD as i32),
        "the probe must fail specifically for absent symlink privilege: {error}"
    );
    println!(
        "{}",
        serde_json::json!({
            "symlinkPrivilegePresentBefore": present,
            "symlinkPrivilegePresentAfter": false,
            "stdSymlinkDirError": error.raw_os_error(),
        })
    );
}

#[test]
fn native_windows_companion_without_symlink_privilege() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        // The parent assigns this process and all descendants to its private Job
        // before permitting token mutation or any product command.
        let mut ready = [0];
        io::stdin().read_exact(&mut ready).unwrap();
        assert_eq!(ready, [b'1']);
        let root = Path::new(&root);
        remove_child_symlink_privilege(root);
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/windows_companion_bindings.cjs");
        let binary = ocm_test_binary_path();
        println!(
            "{}",
            serde_json::json!({
                "command": "node", "args": [script, binary, root],
                "scriptSha256": file_sha256(&script).unwrap(),
                "binarySha256": file_sha256(&binary).unwrap(),
            })
        );
        let status = Command::new("node")
            .arg(&script)
            .arg(&binary)
            .arg(root)
            .stdin(Stdio::null())
            .status()
            .unwrap();
        println!("Node fixture exit: {status}");
        assert!(
            status.success(),
            "native companion fixture failed: {status}"
        );
        return;
    }

    let fixture = TestDir::new("native-windows-companion-bindings");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let binary = fs::canonicalize(ocm_test_binary_path()).unwrap();
    let test_binary = std::env::current_exe().unwrap();
    // Read toolchain versions before isolating USERPROFILE, which also changes
    // rustup's default configuration lookup. No compiler runs in the fixture.
    for tool in ["rustc", "cargo"] {
        let output = Command::new(tool).arg("--version").output().unwrap();
        println!(
            "{}",
            serde_json::json!({
                "command": tool, "args": ["--version"],
                "exit": output.status.code(),
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
            })
        );
        assert!(output.status.success(), "cannot identify {tool}");
    }
    let parent_token = process_token(TOKEN_QUERY).unwrap();
    let luid = symlink_privilege().unwrap();
    let parent_had_privilege = has_privilege(&parent_token, &luid).unwrap();
    let mut env = ocm_env(&fixture);
    for key in ["PATH", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.into(), value);
        }
    }
    for (key, relative) in [
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("APPDATA", "home/AppData/Roaming"),
        ("LOCALAPPDATA", "home/AppData/Local"),
        ("TEMP", "tmp"),
        ("TMP", "tmp"),
    ] {
        let directory = fixture.child(relative);
        fs::create_dir_all(&directory).unwrap();
        env.insert(key.into(), path_string(&directory));
    }
    env.insert("OCM_TEST_BINARY".into(), path_string(&binary));
    env.insert(CHILD_ROOT.into(), path_string(fixture.path()));
    let args = ["--exact", TEST_NAME, "--nocapture", "--test-threads=1"];
    println!(
        "{}",
        serde_json::json!({
            "command": test_binary, "args": args,
            "testBinarySha256": file_sha256(&test_binary).unwrap(),
            "fixtureSourceSha256": file_sha256(&repo.join("tests/windows_companion_bindings_tests.rs")).unwrap(),
            "ocmBinary": binary, "ocmBinarySha256": file_sha256(&binary).unwrap(),
        })
    );
    let output_path = fixture.child("proof.log");
    let output = fs::File::create(&output_path).unwrap();
    let job = FixtureJob::new().expect("create private companion fixture job");
    let mut child = Command::new(&test_binary)
        .args(args)
        .current_dir(repo)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .spawn()
        .expect("start isolated privilege child");
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{error}");
    }
    child.stdin.take().unwrap().write_all(b"1").unwrap();
    let deadline = Instant::now() + Duration::from_secs(240);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error.to_string()),
            Ok(None) if Instant::now() >= deadline => {
                break Err("companion fixture timed out".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    let cleanup = job.stop();
    let _ = child.wait();
    let diagnostics = fs::read_to_string(&output_path).unwrap();
    println!("{diagnostics}\nIsolated privilege child exit: {result:?}");
    assert_eq!(
        has_privilege(&parent_token, &luid).unwrap(),
        parent_had_privilege,
        "isolated child changed the parent token"
    );
    cleanup.expect("settle all companion fixture descendants");
    assert!(
        matches!(&result, Ok(status) if status.success()),
        "{result:?}\n{diagnostics}"
    );
}
