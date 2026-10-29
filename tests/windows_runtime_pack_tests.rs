#![cfg(windows)]

mod support;

use std::fs;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE, STILL_ACTIVE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use support::{
    TestDir, ocm_env, ocm_test_binary_path, path_string, run_ocm, stderr, windows_job::FixtureJob,
};

fn pin_stage_holder(owner: u32, executable: &str) -> io::Result<Option<OwnedHandle>> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W::default();
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    let mut found = None;
    let mut available = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    let mut last_error = if available == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    while available != 0 {
        let end = entry
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
        if entry.th32ParentProcessID == owner && name.eq_ignore_ascii_case(executable) {
            if found.is_some() {
                return Err(io::Error::other("ambiguous stage holder"));
            }
            let process =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID) };
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            found = Some(unsafe { OwnedHandle::from_raw_handle(process) });
        }
        available = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
        if available == 0 {
            last_error = unsafe { GetLastError() };
        }
    }
    if last_error != ERROR_NO_MORE_FILES {
        return Err(io::Error::from_raw_os_error(last_error as i32));
    }
    Ok(found)
}

#[test]
fn native_windows_pack_preserves_live_child_archive() {
    let fixture = TestDir::new("native-windows-runtime-pack & %PATH% !literal! 雪");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output_path = fixture.child("proof.log");
    let output = fs::File::create(&output_path).unwrap();
    let mut command = Command::new("node");
    command
        .current_dir(repo)
        .arg(repo.join("tests/support/windows_runtime_pack.cjs"))
        .arg(ocm_test_binary_path())
        .arg(fixture.path())
        .env_clear()
        .envs(ocm_env(&fixture))
        .stdin(Stdio::piped())
        .stdout(output.try_clone().unwrap())
        .stderr(output);
    for key in ["PATH", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("TEMP", fixture.path())
        .env("TMP", fixture.path());
    let job = FixtureJob::new().expect("create private packing fixture job");
    let mut child = command.spawn().expect("start native packing fixture");
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{error}");
    }
    // The helper must not spawn OCM until all descendants belong to our Job.
    child.stdin.take().unwrap().write_all(b"owned\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let executable = ocm_test_binary_path();
    let executable = executable
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut holder = None;
    let mut holder_terminated = false;
    let result = loop {
        if holder.is_none() {
            if let Some(owner) = fs::read_to_string(fixture.child("owner-pid"))
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
            {
                match pin_stage_holder(owner, &executable) {
                    Ok(Some(process)) => {
                        holder = Some(process);
                        fs::write(fixture.child("helper-observed"), "observed").unwrap();
                    }
                    Ok(None) => {}
                    Err(error) => break Err(error.to_string()),
                }
            }
        }
        if !holder_terminated && fixture.child("owner-killed").exists() {
            if let Some(process) = &holder {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
                    break Err(io::Error::last_os_error().to_string());
                }
                if code != STILL_ACTIVE as u32 {
                    holder_terminated = true;
                    fs::write(fixture.child("helper-terminated"), "terminated").unwrap();
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error.to_string()),
            Ok(None) if Instant::now() >= deadline => break Err("packing fixture timed out".into()),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Parent-only termination is part of the scenario. Reap every surviving
    // descendant only after its assertions, including on test failure.
    job.stop().expect("settle every packing fixture process");
    let _ = child.wait();
    let diagnostics = fs::read_to_string(output_path).unwrap();
    println!("{diagnostics}");
    assert!(
        matches!(&result, Ok(status) if status.success()),
        "{result:?}\n{diagnostics}"
    );

    assert!(holder_terminated, "the inert holder survived its owner");

    // The original active-child assertion ran before Job cleanup. With all
    // descendants now proven stopped, retry must reclaim its abandoned stage.
    let mut env = ocm_env(&fixture);
    for key in ["WINDIR", "COMSPEC", "PATHEXT"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.into(), value);
        }
    }
    env.insert("USERPROFILE".into(), path_string(&fixture.child("home")));
    env.insert("APPDATA".into(), path_string(&fixture.child("appdata")));
    env.insert(
        "LOCALAPPDATA".into(),
        path_string(&fixture.child("localappdata")),
    );
    env.insert("TEMP".into(), path_string(fixture.path()));
    env.insert("TMP".into(), path_string(fixture.path()));
    env.insert("npm_config_offline".into(), "true".into());
    env.insert(
        "npm_config_cache".into(),
        path_string(&fixture.child("cache")),
    );
    env.insert(
        "OCM_INTERNAL_NPM_BIN".into(),
        fs::read_to_string(fixture.child("npm-bin")).unwrap(),
    );
    let source = path_string(&fixture.child("repo & %PATH% !literal! 雪"));
    let args = [
        "runtime",
        "build-local",
        "stable",
        "--repo",
        &source,
        "--force",
        "--json",
    ];
    let retry = run_ocm(fixture.path(), &env, &args);
    assert!(retry.status.success(), "{}", stderr(&retry));
    let runtimes = fixture.child("ocm-home/runtimes");
    let no_stages = || {
        let mut first_remaining_stage = None;
        let clear = fs::read_dir(&runtimes)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| {
                let name = entry.file_name();
                let clear = !name.to_string_lossy().starts_with(".stable.stage-");
                if !clear {
                    first_remaining_stage = Some(name);
                }
                clear
            });
        (clear, first_remaining_stage)
    };
    // Latch the original predicate before any added output or filesystem read.
    let (clear, first_remaining_stage) = no_stages();
    if let Some(name) = &first_remaining_stage {
        eprintln!("first cleanup observation: clear={clear}, remaining={name:?}");
        eprintln!(
            "original owner PID: {:?}",
            fs::read_to_string(fixture.child("owner-pid"))
        );
        eprintln!("retry stderr: {}", stderr(&retry));
        let remaining = runtimes.join(name);
        eprintln!(
            "later residue kind (directory, file, symlink): {:?}",
            fs::symlink_metadata(&remaining).map(|meta| (
                meta.is_dir(),
                meta.is_file(),
                meta.file_type().is_symlink(),
            ))
        );
        match fs::read_dir(&remaining) {
            Ok(entries) => {
                for entry in entries.take(16) {
                    eprintln!(
                        "later residue child: {:?}",
                        entry.map(|entry| entry.file_name())
                    );
                }
            }
            Err(error) => eprintln!("later residue enumeration: {error:?}"),
        }
    }
    assert!(
        clear,
        "first cleanup observation retained {first_remaining_stage:?}"
    );
    assert!(!runtimes.join("stable/pack").exists());
    let verify = run_ocm(
        fixture.path(),
        &env,
        &["runtime", "verify", "stable", "--json"],
    );
    assert!(verify.status.success(), "{}", stderr(&verify));
    env.insert("PACK_FAIL".into(), "1".into());
    let failure = run_ocm(fixture.path(), &env, &args);
    assert!(!failure.status.success());
    assert!(stderr(&failure).contains("failed to pack local OpenClaw build"));
    assert!(no_stages().0);
    let verify = run_ocm(
        fixture.path(),
        &env,
        &["runtime", "verify", "stable", "--json"],
    );
    assert!(verify.status.success(), "{}", stderr(&verify));
    let remove = run_ocm(
        fixture.path(),
        &env,
        &["runtime", "remove", "stable", "--json"],
    );
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(no_stages().0);
    println!("native Windows retry, publication, error preservation and removal passed");
}
