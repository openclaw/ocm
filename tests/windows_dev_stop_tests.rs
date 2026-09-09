#![cfg(windows)]

mod support;

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{FILETIME, HANDLE, STILL_ACTIVE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, TerminateProcess,
};

use crate::support::{TestDir, ocm_env, ocm_test_binary_path};

fn windows_error(action: &str) -> String {
    format!("{action}: {}", io::Error::last_os_error())
}

struct FixtureJob(OwnedHandle);

impl FixtureJob {
    fn new() -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(windows_error("create fixture job"));
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(windows_error("configure fixture job"));
        }
        Ok(job)
    }

    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle()
    }

    fn assign(&self, child: &Child) -> Result<(), String> {
        if unsafe { AssignProcessToJobObject(self.handle(), child.as_raw_handle()) } == 0 {
            return Err(windows_error("assign Node to fixture job"));
        }
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        if unsafe { TerminateJobObject(self.handle(), 1) } == 0 {
            return Err(windows_error("stop fixture job"));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if unsafe {
                QueryInformationJobObject(
                    self.handle(),
                    JobObjectBasicAccountingInformation,
                    std::ptr::from_mut(&mut info).cast(),
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(windows_error("inspect fixture job cleanup"));
            }
            if info.ActiveProcesses == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "fixture job retained {} processes",
                    info.ActiveProcesses
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn process_running(handle: HANDLE) -> Result<bool, String> {
    let mut code = 0;
    if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
        return Err(windows_error("read fixture process status"));
    }
    Ok(code == STILL_ACTIVE as u32)
}

struct FixtureProcesses {
    handles: BTreeMap<u32, (OwnedHandle, String)>,
}

impl FixtureProcesses {
    fn request(&mut self, job: &FixtureJob, request: &Value) -> Result<Value, String> {
        let pid = request["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
            .ok_or("invalid fixture PID")?;
        if request["op"] == "capture" {
            if !self.handles.contains_key(&pid) {
                let raw = unsafe {
                    OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                        0,
                        pid,
                    )
                };
                if raw.is_null() {
                    return Err(windows_error("open fixture process"));
                }
                let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
                let mut belongs = 0;
                if unsafe { IsProcessInJob(raw, job.handle(), &mut belongs) } == 0 {
                    return Err(windows_error("verify fixture process ownership"));
                }
                if belongs == 0 {
                    return Err("refusing a process outside the private fixture job".to_string());
                }
                let mut created = FILETIME::default();
                let mut exited = FILETIME::default();
                let mut kernel = FILETIME::default();
                let mut user = FILETIME::default();
                if unsafe {
                    GetProcessTimes(raw, &mut created, &mut exited, &mut kernel, &mut user)
                } == 0
                {
                    return Err(windows_error("read fixture process identity"));
                }
                let started_at = (((created.dwHighDateTime as u64) << 32)
                    | created.dwLowDateTime as u64)
                    .to_string();
                self.handles.insert(pid, (handle, started_at));
            }
            let (handle, started_at) = &self.handles[&pid];
            return Ok(
                json!({"startedAt": started_at, "running": process_running(handle.as_raw_handle())?}),
            );
        }
        let (handle, started_at) = self
            .handles
            .get(&pid)
            .ok_or("fixture process was not captured")?;
        if request["startedAt"].as_str() != Some(started_at.as_str()) {
            return Err("fixture process identity does not match its owned handle".to_string());
        }
        match request["op"].as_str() {
            Some("running") => Ok(json!({"running": process_running(handle.as_raw_handle())?})),
            Some("terminate") => {
                if !process_running(handle.as_raw_handle())? {
                    return Ok(json!({"stopped": false}));
                }
                if unsafe { TerminateProcess(handle.as_raw_handle(), 1) } == 0 {
                    if !process_running(handle.as_raw_handle())? {
                        return Ok(json!({"stopped": false}));
                    }
                    return Err(windows_error("terminate owned fixture process"));
                }
                let deadline = Instant::now() + Duration::from_secs(5);
                while process_running(handle.as_raw_handle())? {
                    if Instant::now() >= deadline {
                        return Err("owned fixture process did not exit".to_string());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(json!({"stopped": true}))
            }
            _ => Err("unknown native fixture operation".to_string()),
        }
    }
}

enum Output {
    Line(Vec<u8>),
    Stderr(Vec<u8>),
    StdoutClosed,
    StderrClosed,
    Failed(String),
}

fn read_output(mut reader: impl Read, stderr: bool, sender: SyncSender<Output>) {
    let mut pending = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) => {
                let result = if stderr {
                    Output::StderrClosed
                } else if pending.is_empty() {
                    Output::StdoutClosed
                } else {
                    Output::Failed("unterminated fixture output".to_string())
                };
                let _ = sender.send(result);
                return;
            }
            Ok(count) => count,
            Err(error) => {
                let _ = sender.send(Output::Failed(error.to_string()));
                return;
            }
        };
        if stderr {
            if sender
                .send(Output::Stderr(chunk[..count].to_vec()))
                .is_err()
            {
                return;
            }
            continue;
        }
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                if sender
                    .send(Output::Line(std::mem::take(&mut pending)))
                    .is_err()
                {
                    return;
                }
            } else {
                pending.push(*byte);
                if pending.len() > 65536 {
                    let _ = sender.send(Output::Failed(
                        "fixture output exceeded its bound".to_string(),
                    ));
                    return;
                }
            }
        }
    }
}

fn run_helper(
    child: &mut Child,
    input: &mut ChildStdin,
    output: &Receiver<Output>,
    job: &FixtureJob,
    stderr: &mut Vec<u8>,
) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut processes = FixtureProcesses {
        handles: BTreeMap::new(),
    };
    let mut next_id = 1;
    let mut ready = false;
    let mut result = None;
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        if Instant::now() >= deadline {
            return Err("native Windows helper exceeded its 180-second bound".to_string());
        }
        if stdout_closed && stderr_closed {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                if !status.success() {
                    return Err(format!("native Windows helper failed ({status})"));
                }
                return result
                    .ok_or_else(|| "native Windows helper omitted its result".to_string());
            }
        }
        match output.recv_timeout(Duration::from_millis(50)) {
            Ok(Output::Line(line)) => {
                let value: Value = serde_json::from_slice(&line)
                    .map_err(|error| format!("invalid fixture output: {error}"))?;
                if value["nativeWindowsStopProof"] == 1 {
                    if value["id"].as_u64() != Some(next_id) || result.is_some() {
                        return Err("out-of-order native fixture request".to_string());
                    }
                    let response = if !ready {
                        if value["op"] != "ready" {
                            return Err("fixture omitted ownership handshake".to_string());
                        }
                        ready = true;
                        Ok(json!({"ready": true}))
                    } else {
                        processes.request(job, &value)
                    };
                    let reply = match response {
                        Ok(value) => json!({"id": next_id, "ok": true, "result": value}),
                        Err(error) => json!({"id": next_id, "ok": false, "error": error}),
                    };
                    let reply = reply.to_string();
                    if reply.len() > 4096 {
                        return Err("native fixture response exceeded its bound".to_string());
                    }
                    writeln!(input, "{reply}").map_err(|error| error.to_string())?;
                    input.flush().map_err(|error| error.to_string())?;
                    next_id += 1;
                } else if ready && result.is_none() && value["platform"] == "win32" {
                    result = Some(value);
                } else {
                    return Err("unexpected native fixture output".to_string());
                }
            }
            Ok(Output::Stderr(bytes)) => {
                stderr.extend(bytes);
                if stderr.len() > 24000 {
                    stderr.drain(..stderr.len() - 24000);
                }
            }
            Ok(Output::StdoutClosed) => stdout_closed = true,
            Ok(Output::StderrClosed) => stderr_closed = true,
            Ok(Output::Failed(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if stdout_closed => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("fixture output channel closed unexpectedly".to_string());
            }
        }
    }
}

#[test]
fn native_windows_dev_stop_lifecycle() {
    let fixture = TestDir::new("native-windows-dev-stop");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let helper = repo.join("tests/support/windows_dev_stop.cjs");
    let mut command = Command::new("node");
    command
        // Node waits for the wrapper's ownership acknowledgement before spawning.
        // Its cwd stays outside the fixture so Windows permits fixture removal.
        .current_dir(repo)
        .arg(helper)
        .arg(ocm_test_binary_path())
        .arg(fixture.path())
        .env_clear()
        .envs(ocm_env(&fixture))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let job = FixtureJob::new().expect("create private native fixture job");
    let mut child = command
        .spawn()
        .expect("run the native Windows dev-stop helper");
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{error}");
    }
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, output) = mpsc::sync_channel(32);
    let stderr_sender = sender.clone();
    let readers = [
        thread::spawn(move || read_output(stdout, false, sender)),
        thread::spawn(move || read_output(stderr, true, stderr_sender)),
    ];
    let mut diagnostics = Vec::new();
    let result = run_helper(&mut child, &mut input, &output, &job, &mut diagnostics);
    drop(input);
    drop(output);
    // This fallback happens only after the helper's product assertions or failure.
    // Keep the outer job open throughout the named-stop and crash-recovery cases.
    let cleanup = job.stop();
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() && Instant::now() < exit_deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let node_exited = child.try_wait().unwrap().is_some();
    let read_deadline = Instant::now() + Duration::from_secs(5);
    while readers.iter().any(|reader| !reader.is_finished()) && Instant::now() < read_deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let readers_finished = readers.iter().all(|reader| reader.is_finished());
    for reader in readers {
        if reader.is_finished() {
            reader.join().expect("fixture output reader");
        }
    }
    cleanup.expect("stop every process owned by the native fixture");
    assert!(
        node_exited,
        "native Windows helper did not exit after cleanup"
    );
    assert!(
        readers_finished,
        "native Windows fixture output did not close after cleanup"
    );
    let result =
        result.unwrap_or_else(|error| panic!("{error}\n{}", String::from_utf8_lossy(&diagnostics)));
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
