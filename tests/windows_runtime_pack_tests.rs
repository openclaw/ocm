#![cfg(windows)]

mod support;

use std::fs;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, FILETIME, GetLastError, INVALID_HANDLE_VALUE, STILL_ACTIVE, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

use support::{
    TestDir, ocm_env, ocm_test_binary_path, path_string, run_ocm, stderr, windows_job::FixtureJob,
};

#[derive(serde::Deserialize)]
struct PackIdentity {
    pid: u32,
    nonce: String,
    proof: Option<String>,
}

#[derive(Debug)]
struct ProcessObservation {
    wait: u32,
    wait_error: Option<u32>,
    exit: Option<u32>,
    exit_error: Option<u32>,
}

impl ProcessObservation {
    fn signaled(&self) -> bool {
        self.wait == WAIT_OBJECT_0
    }

    fn live(&self) -> bool {
        self.wait == WAIT_TIMEOUT && self.exit == Some(STILL_ACTIVE as u32)
    }
}

struct PinnedPostpack {
    handle: OwnedHandle,
    pid: u32,
    created: u64,
    proof: String,
    authenticated: bool,
}

impl PinnedPostpack {
    fn open(identity: &PackIdentity) -> Result<Self, String> {
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                0,
                identity.pid,
            )
        };
        if raw.is_null() {
            return Err(format!(
                "open postpack {}: {}",
                identity.pid,
                io::Error::last_os_error()
            ));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe { GetProcessTimes(raw, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
            return Err(format!(
                "postpack creation identity: {}",
                io::Error::last_os_error()
            ));
        }
        let created = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        Ok(Self {
            handle,
            pid: identity.pid,
            created,
            proof: format!("{}:{}:{created}", identity.nonce, identity.pid),
            authenticated: false,
        })
    }

    fn observe(&self, timeout_ms: u32) -> ProcessObservation {
        let wait = unsafe { WaitForSingleObject(self.handle.as_raw_handle(), timeout_ms) };
        let wait_error = (wait == WAIT_FAILED).then(|| unsafe { GetLastError() });
        let mut exit = 0;
        let queried = unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut exit) };
        let exit_error = (queried == 0).then(|| unsafe { GetLastError() });
        ProcessObservation {
            wait,
            wait_error,
            exit: (queried != 0).then_some(exit),
            exit_error,
        }
    }

    fn qualify_live_member(&self, job: &FixtureJob) -> Result<ProcessObservation, String> {
        if !self.authenticated {
            return Err("postpack handle has no fresh child acknowledgement".into());
        }
        let mut member = 0;
        if unsafe { IsProcessInJob(self.handle.as_raw_handle(), job.handle(), &mut member) } == 0 {
            return Err(format!(
                "postpack membership query: {}",
                io::Error::last_os_error()
            ));
        }
        if member == 0 {
            return Err(format!(
                "verified postpack pid={} created={} membership=false in the specific FixtureJob",
                self.pid, self.created
            ));
        }
        let observed = self.observe(0);
        if !observed.live() {
            return Err(format!("postpack not live at qualification: {observed:?}"));
        }
        Ok(observed)
    }
}

// This owns the fixture until the added held child has been proven settled.
// No assertion is raised in the armed interval before this finalizer runs.
struct PackCapture {
    fixture: Option<TestDir>,
    job: FixtureJob,
    helper: Child,
    nonce: Option<String>,
    postpack: Option<PinnedPostpack>,
    containment_confirmed: bool,
    stop_attempted: bool,
    stop_ok: bool,
    settled: bool,
    rescue_attempted: bool,
}

impl PackCapture {
    fn path(&self, suffix: &str) -> Result<std::path::PathBuf, String> {
        let nonce = self.nonce.as_deref().ok_or("capture nonce unavailable")?;
        Ok(self
            .fixture
            .as_ref()
            .unwrap()
            .child(format!("postpack-{nonce}.{suffix}")))
    }

    fn identity(&self, suffix: &str) -> Result<PackIdentity, String> {
        let bytes = fs::read(self.path(suffix)?).map_err(|error| error.to_string())?;
        let identity: PackIdentity =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if identity.pid == 0 || Some(identity.nonce.as_str()) != self.nonce.as_deref() {
            return Err("postpack PID/nonce does not match this fixture".into());
        }
        Ok(identity)
    }

    fn write_once(&self, suffix: &str, value: serde_json::Value) -> Result<(), String> {
        let path = self.path(suffix)?;
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                        .map_err(|error| error.to_string())?;
                return if existing["nonce"].as_str() == self.nonce.as_deref() {
                    Ok(())
                } else {
                    Err("foreign capture marker".into())
                };
            }
            Err(error) => return Err(error.to_string()),
        };
        file.write_all(value.to_string().as_bytes())
            .map_err(|error| error.to_string())
    }

    fn release(&self) -> Result<(), String> {
        self.write_once("release", serde_json::json!({"nonce": self.nonce}))
    }

    fn stop(&mut self) -> Result<support::windows_job::StopAccounting, String> {
        self.stop_attempted = true;
        let result = self.job.stop();
        self.stop_ok = result.is_ok();
        result
    }

    fn rescue(&mut self) {
        if self.settled || self.rescue_attempted {
            return;
        }
        self.rescue_attempted = true;
        let release = self.release();
        let done = self.write_once(
            "done",
            serde_json::json!({"nonce": self.nonce, "hold": false}),
        );
        let stop = if self.stop_attempted {
            None
        } else {
            Some(self.stop())
        };
        let mut helper_wait = unsafe { WaitForSingleObject(self.helper.as_raw_handle(), 10_000) };
        if helper_wait != WAIT_OBJECT_0 {
            let _ = self.helper.kill();
            helper_wait = unsafe { WaitForSingleObject(self.helper.as_raw_handle(), 10_000) };
        }
        if helper_wait == WAIT_OBJECT_0 {
            let _ = self.helper.wait();
        }
        let mut termination = None;
        let observed = self
            .postpack
            .as_ref()
            .filter(|process| process.authenticated)
            .map(|process| {
                let mut observed = process.observe(10_000);
                if !observed.signaled() {
                    let killed = unsafe { TerminateProcess(process.handle.as_raw_handle(), 1) };
                    termination = Some((killed, (killed == 0).then(|| unsafe { GetLastError() })));
                    observed = process.observe(10_000);
                }
                observed
            });
        let rescue_settled = self.containment_confirmed
            && self.stop_ok
            && helper_wait == WAIT_OBJECT_0
            && observed.as_ref().is_some_and(ProcessObservation::signaled);
        // Rescue does not establish the original descendant-settlement sequence.
        // Keep the fixture on failure, even when the pinned child is now stopped.
        eprintln!(
            "failure-only settlement: release={release:?}, done={done:?}, stop={stop:?}, helper_wait={helper_wait}, termination={termination:?}, postpack={observed:?}, rescued_child_settled={rescue_settled}"
        );
    }
}

impl Drop for PackCapture {
    fn drop(&mut self) {
        self.rescue();
        if !self.settled {
            if let Some(fixture) = self.fixture.take() {
                eprintln!(
                    "cleanup unconfirmed; retaining fixture {:?}",
                    fixture.path()
                );
                std::mem::forget(fixture);
            }
        }
    }
}

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
    // The matched observer control must qualify before the forced-tail arm.
    run_pack_capture(true);
    run_pack_capture(false);
}

fn run_pack_capture(natural_release: bool) {
    let fixture = TestDir::new("native-windows-runtime-pack & %PATH% !literal! 雪");
    let root = fixture.path().to_path_buf();
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
    drop(command);
    let mut capture = PackCapture {
        fixture: Some(fixture),
        job,
        helper: child,
        nonce: None,
        postpack: None,
        containment_confirmed: false,
        stop_attempted: false,
        stop_ok: false,
        settled: false,
        rescue_attempted: false,
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    let executable = ocm_test_binary_path();
    let executable = executable
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut holder = None;
    let mut holder_terminated = false;
    let mut armed = false;
    let result = (|| -> Result<std::process::ExitStatus, String> {
        // No descendant is allowed to start before the helper is assigned.
        capture
            .helper
            .stdin
            .take()
            .ok_or("helper stdin missing")?
            .write_all(b"owned\n")
            .map_err(|error| error.to_string())?;
        loop {
            if holder.is_none() {
                if let Some(owner) = fs::read_to_string(root.join("owner-pid"))
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok())
                {
                    if let Some(process) =
                        pin_stage_holder(owner, &executable).map_err(|error| error.to_string())?
                    {
                        holder = Some(process);
                        let nonce = fs::read_to_string(root.join("capture-nonce"))
                            .map_err(|error| error.to_string())?;
                        if nonce.len() != 36
                            || !nonce.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
                        {
                            return Err("invalid fixture capture nonce".into());
                        }
                        capture.nonce = Some(nonce);
                        let identity = capture.identity("ready")?;
                        // A PID record alone does not authenticate this handle:
                        // the original child could have exited and its PID reused.
                        capture.postpack = Some(PinnedPostpack::open(&identity)?);
                        capture.write_once(
                            "challenge",
                            serde_json::json!({
                                "pid": identity.pid, "nonce": identity.nonce,
                                "proof": capture.postpack.as_ref().unwrap().proof
                            }),
                        )?;
                    }
                }
            }
            if !armed && capture.postpack.is_some() && capture.path("pinned")?.exists() {
                let acknowledgement = capture.identity("pinned")?;
                let process = capture.postpack.as_mut().unwrap();
                if acknowledgement.pid != process.pid
                    || acknowledgement.proof.as_deref() != Some(process.proof.as_str())
                    || !process.observe(0).live()
                {
                    return Err(
                        "post-open acknowledgement did not authenticate the live handle".into(),
                    );
                }
                // The actual child answered a challenge created after this
                // handle opened. Keep this owned handle even if membership=false.
                process.authenticated = true;
                process.qualify_live_member(&capture.job)?;
                capture.containment_confirmed = true;
                capture.write_once(
                    "armed",
                    serde_json::json!({
                        "pid": acknowledgement.pid, "nonce": acknowledgement.nonce
                    }),
                )?;
                armed = true;
            }
            if !holder_terminated && root.join("owner-killed").exists() {
                if let Some(process) = &holder {
                    let mut code = 0;
                    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
                        return Err(io::Error::last_os_error().to_string());
                    }
                    if code != STILL_ACTIVE as u32 {
                        holder_terminated = true;
                        fs::write(root.join("helper-terminated"), "terminated")
                            .map_err(|error| error.to_string())?;
                    }
                }
            }
            match capture
                .helper
                .try_wait()
                .map_err(|error| error.to_string())?
            {
                Some(status) => return Ok(status),
                None if Instant::now() >= deadline => {
                    return Err("packing fixture timed out".into());
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    })();
    let qualification = (|| -> Result<ProcessObservation, String> {
        if !matches!(&result, Ok(status) if status.success()) || !holder_terminated {
            return Err("original owner/holder/live-archive scenario did not qualify".into());
        }
        capture.containment_confirmed = false;
        let held = capture.identity("held")?;
        let process = capture.postpack.as_ref().ok_or("postpack was not pinned")?;
        if held.pid != process.pid {
            return Err("held announcement changed the pinned process identity".into());
        }
        let before = process.qualify_live_member(&capture.job)?;
        capture.containment_confirmed = true;
        if natural_release {
            capture.release()?;
            let released = process.observe(10_000);
            if !released.signaled() || released.exit != Some(0) {
                return Err(format!(
                    "natural-release control did not exit0: {released:?}"
                ));
            }
            Ok(released)
        } else {
            Ok(before)
        }
    })();
    let stopped = capture.stop();
    // The first observation after the existing stop is a zero-time exact-handle
    // wait and exit-code read. No output, rescue, or additional wait precedes it.
    let observed = capture
        .postpack
        .as_ref()
        .filter(|process| process.authenticated)
        .map(|process| process.observe(0));
    let helper_wait = unsafe { WaitForSingleObject(capture.helper.as_raw_handle(), 10_000) };
    if helper_wait == WAIT_OBJECT_0 {
        let _ = capture.helper.wait();
    }
    capture.settled = capture.containment_confirmed
        && stopped.is_ok()
        && helper_wait == WAIT_OBJECT_0
        && observed.as_ref().is_some_and(ProcessObservation::signaled);
    let valid = qualification.is_ok()
        && capture.settled
        && observed.as_ref().is_some_and(|value| value.exit.is_some());
    if !valid {
        capture.rescue();
    }
    let identity = capture
        .postpack
        .as_ref()
        .map(|process| (process.pid, process.created, process.authenticated));
    // Both passing arms close the same extra observer before ordinary retry.
    if valid {
        drop(capture.postpack.take());
    }
    let diagnostics = fs::read_to_string(&output_path).unwrap_or_else(|error| error.to_string());
    println!(
        "postpack capture natural_release={natural_release} identity={identity:?} nonce={:?} qualification={qualification:?} stop={stopped:?} immediate={observed:?} rescue={}\n{diagnostics}",
        capture.nonce, capture.rescue_attempted
    );
    assert!(
        valid,
        "paired capture did not qualify: result={result:?}, qualification={qualification:?}, stop={stopped:?}, immediate={observed:?}"
    );
    let fixture = capture.fixture.as_ref().unwrap();

    // The original active-child assertion ran before Job cleanup. With all
    // descendants now proven stopped, retry must reclaim its abandoned stage.
    let mut env = ocm_env(fixture);
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
