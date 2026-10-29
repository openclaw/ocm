#![cfg(windows)]

mod support;

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CompareObjectHandles, ERROR_MORE_DATA, FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    IsProcessInJob, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList, QueryInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};

use support::{
    TestDir, ocm_env, ocm_test_binary_path, path_string, run_ocm, stderr, windows_job::FixtureJob,
};

type Result<T> = std::result::Result<T, String>;

fn native_error(action: &str) -> String {
    format!("{action}: {}", io::Error::last_os_error())
}

struct Process {
    handle: OwnedHandle,
    pid: u32,
    created: u64,
}

impl Process {
    fn open(pid: u32, job: &FixtureJob) -> Result<Self> {
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            return Err(native_error("open fixture member"));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut member = 0;
        if unsafe { IsProcessInJob(raw, job.handle(), &mut member) } == 0 {
            return Err(native_error("query fixture membership"));
        }
        if member == 0 {
            return Err(format!("process {pid} is outside the packing fixture Job"));
        }
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe { GetProcessTimes(raw, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
            return Err(native_error("query member creation identity"));
        }
        Ok(Self {
            handle,
            pid,
            created: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        })
    }

    fn live(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            _ => Err(native_error("inspect packing process signal")),
        }
    }

    fn wait(&self, deadline: Instant) -> Result<()> {
        wait_handle(self.handle.as_raw_handle(), deadline)
            .map_err(|error| format!("process {} created {}: {error}", self.pid, self.created))
    }

    fn image_name(&self) -> Result<String> {
        let mut path = vec![0u16; 32768];
        let mut len = path.len() as u32;
        if unsafe {
            QueryFullProcessImageNameW(self.handle.as_raw_handle(), 0, path.as_mut_ptr(), &mut len)
        } == 0
        {
            return Err(native_error("query fixture member image"));
        }
        Ok(String::from_utf16_lossy(&path[..len as usize]))
    }
}

fn wait_handle(handle: windows_sys::Win32::Foundation::HANDLE, deadline: Instant) -> Result<()> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_millis();
    let timeout = remaining.min(u128::from(u32::MAX - 1)) as u32;
    match unsafe { WaitForSingleObject(handle, timeout) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err("packing process did not signal before the shared deadline".into()),
        _ => Err(native_error("wait for packing process")),
    }
}

fn total_processes(job: &FixtureJob) -> Result<u32> {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    if unsafe {
        QueryInformationJobObject(
            job.handle(),
            JobObjectBasicAccountingInformation,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of_val(&info) as u32,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(native_error("query packing Job accounting"));
    }
    Ok(info.TotalProcesses)
}

fn member_pids(job: &FixtureJob) -> Result<Vec<u32>> {
    // usize storage supplies the structure's native alignment, including its
    // variable ULONG_PTR array. Start small to exercise resize on this fixture.
    let offset = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
    let mut capacity = 1usize;
    loop {
        let bytes = offset + capacity * std::mem::size_of::<usize>();
        let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let ok = unsafe {
            QueryInformationJobObject(
                job.handle(),
                JobObjectBasicProcessIdList,
                storage.as_mut_ptr().cast(),
                bytes as u32,
                std::ptr::null_mut(),
            )
        };
        let error = (ok == 0).then(io::Error::last_os_error);
        let info = unsafe { &*storage.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
        let assigned = info.NumberOfAssignedProcesses as usize;
        let listed = info.NumberOfProcessIdsInList as usize;
        if let Some(error) = &error {
            if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(format!("list packing Job processes: {error}"));
            }
        }
        if listed > capacity || listed > assigned {
            return Err("inconsistent packing Job process list".into());
        }
        if error.is_none() && assigned == listed {
            let ids = unsafe {
                std::slice::from_raw_parts(
                    storage.as_ptr().cast::<u8>().add(offset).cast::<usize>(),
                    listed,
                )
            };
            let mut seen = BTreeSet::new();
            for &id in ids {
                let pid = u32::try_from(id).map_err(|_| "invalid Job process ID")?;
                if pid == 0 || !seen.insert(pid) {
                    return Err("invalid/duplicate Job process ID".into());
                }
            }
            return Ok(seen.into_iter().collect());
        }
        // A bounded fixture cannot need an unbounded allocation or retry loop.
        capacity = capacity.saturating_mul(2).max(assigned);
        if capacity > 4096 {
            return Err("packing Job process list did not fit within its capture bound".into());
        }
    }
}

#[derive(Default)]
struct Cohort {
    processes: Vec<Process>,
    before_pin: Option<u32>,
    expected_total: u32,
}

impl Cohort {
    fn capture(&mut self, job: &FixtureJob, completed: u32, allow_new: bool) -> Result<()> {
        let before = total_processes(job)?;
        for pid in member_pids(job)? {
            let process = Process::open(pid, job)?;
            if let Some(previous) = self.processes.iter().find(|old| old.pid == pid) {
                if unsafe {
                    CompareObjectHandles(
                        previous.handle.as_raw_handle(),
                        process.handle.as_raw_handle(),
                    )
                } == 0
                    || previous.created != process.created
                {
                    return Err(format!("packing process identity changed for PID {pid}"));
                }
            } else {
                if !allow_new {
                    return Err(format!("unexpected process {pid} after the postpack gate"));
                }
                if !process.live()? {
                    return Err(format!("process {pid} exited before capture"));
                }
                self.processes.push(process);
            }
        }
        let total = total_processes(job)?;
        if before != total {
            return Err("packing membership changed during gated capture".into());
        }
        if let Some(before_pin) = self.before_pin {
            let expected = before_pin + self.processes.len() as u32 + completed;
            if total != expected {
                return Err(format!(
                    "unaccounted packing processes: total={total}, retained={}, pre-pin={before_pin}, joined={completed}",
                    self.processes.len()
                ));
            }
        } else {
            // The audited npm.cmd reaches its FIRST executable (prefix Node)
            // here. It is held before its script; its shell ancestors are live.
            // The only earlier commands are where.exe and git_short_commit,
            // both before StageJob creation and unable to inherit its pin.
            let before_pin = total
                .checked_sub(self.processes.len() as u32)
                .ok_or("invalid initial Job accounting")?;
            self.before_pin = Some(before_pin);
            println!(
                "packing bootstrap captured {} processes; {before_pin} associations predate the stage pin",
                self.processes.len()
            );
        }
        self.expected_total = total;
        Ok(())
    }

    fn index(&self, pid: u32) -> Result<usize> {
        self.processes
            .iter()
            .position(|process| process.pid == pid)
            .ok_or_else(|| format!("announced process {pid} missing from specific packing Job"))
    }

    fn settle(&self, job: &FixtureJob) -> Result<()> {
        // Termination requests are asynchronous. Accounting zero does not
        // substitute for any process signal (including already-exited members).
        let deadline = Instant::now() + Duration::from_secs(10);
        if unsafe { TerminateJobObject(job.handle(), 1) } == 0 {
            return Err(native_error("stop packing Job"));
        }
        for process in &self.processes {
            process.wait(deadline)?;
        }
        if total_processes(job)? != self.expected_total {
            return Err("a process was born after the final packing capture".into());
        }
        Ok(())
    }
}

struct PackFixture {
    fixture: Option<TestDir>,
    job: FixtureJob,
    helper: Child,
    cohort: Cohort,
    stopped: bool,
    settled: bool,
}

impl Drop for PackFixture {
    fn drop(&mut self) {
        if !self.settled {
            let deadline = Instant::now() + Duration::from_secs(10);
            if !self.stopped {
                unsafe {
                    TerminateJobObject(self.job.handle(), 1);
                }
            }
            for process in &self.cohort.processes {
                let _ = process.wait(deadline);
            }
            let _ = wait_handle(self.helper.as_raw_handle(), deadline);
            // Rescue cannot certify an incomplete cohort. Keep its files even
            // if the known processes have now stopped; Job close is a backstop.
            if let Some(fixture) = self.fixture.take() {
                eprintln!(
                    "packing cleanup unconfirmed; retaining {:?}",
                    fixture.path()
                );
                std::mem::forget(fixture);
            }
        }
    }
}

fn publish(root: &Path, name: &str, value: &Value) -> Result<()> {
    let path = root.join(name);
    let temp = path.with_extension(format!(
        "{}.tmp",
        path.extension().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&temp, value.to_string()).map_err(|error| error.to_string())?;
    fs::rename(temp, path).map_err(|error| error.to_string())
}

fn record(root: &Path, name: &str, helper: &mut Child, deadline: Instant) -> Result<Value> {
    loop {
        match fs::read(root.join(name)) {
            Ok(bytes) => return serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if let Some(status) = helper.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("packing helper exited {status} before {name}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("packing gate timed out: {name}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn identity(value: &Value, nonce: &str) -> Result<u32> {
    if value["nonce"].as_str() != Some(nonce) {
        return Err("packing gate nonce mismatch".into());
    }
    value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| "invalid packing gate PID".into())
}

fn capture_gate(
    capture: &mut PackFixture,
    root: &Path,
    nonce: &str,
    phase: &str,
    deadline: Instant,
) -> Result<(usize, Value)> {
    let name = |suffix: &str| format!("pack-{nonce}.{phase}.{suffix}");
    let ready = record(root, &name("ready"), &mut capture.helper, deadline)?;
    let pid = identity(&ready, nonce)?;
    capture.cohort.capture(&capture.job, 0, true)?;
    let index = capture.cohort.index(pid)?;
    let process = &capture.cohort.processes[index];
    let challenge =
        json!({"nonce":nonce, "pid":pid, "proof":format!("{nonce}:{pid}:{}", process.created)});
    publish(root, &name("challenge"), &challenge)?;
    let pinned = record(root, &name("pinned"), &mut capture.helper, deadline)?;
    if pinned != challenge || !process.live()? {
        return Err(format!(
            "{phase} did not authenticate the retained live process"
        ));
    }
    Ok((index, challenge))
}

#[test]
fn native_windows_pack_preserves_live_child_archive() {
    run_pack_capture(true);
    run_pack_capture(false);
}

fn run_pack_capture(natural_release: bool) {
    let fixture = TestDir::new("native-windows-runtime-pack & %PATH% !literal! 雪");
    let root = fixture.path().to_path_buf();
    let empty = match fs::read_dir(&root) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    };
    if !empty {
        std::mem::forget(fixture);
        panic!("packing fixture root must be new and empty: {root:?}");
    }
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
    let job = FixtureJob::new().expect("create private packing Job");
    let mut child = command.spawn().expect("start native packing fixture");
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{error}");
    }
    drop(command);
    let mut capture = PackFixture {
        fixture: Some(fixture),
        job,
        helper: child,
        cohort: Cohort::default(),
        stopped: false,
        settled: false,
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    let result = (|| -> Result<()> {
        capture
            .helper
            .stdin
            .take()
            .ok_or("helper stdin missing")?
            .write_all(b"owned\n")
            .map_err(|error| error.to_string())?;
        // Nonce is announced before any original-command descendant can start.
        while !root.join("capture-nonce").exists() {
            if capture
                .helper
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
                || Instant::now() >= deadline
            {
                return Err("packing helper did not initialize".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let nonce =
            fs::read_to_string(root.join("capture-nonce")).map_err(|error| error.to_string())?;
        if nonce.len() != 36 || !nonce.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
            return Err("invalid packing nonce".into());
        }
        let (_, bootstrap_release) =
            capture_gate(&mut capture, &root, &nonce, "bootstrap", deadline)?;
        let owner: u32 = fs::read_to_string(root.join("owner-pid"))
            .map_err(|error| error.to_string())?
            .parse()
            .map_err(|_| "invalid owner PID")?;
        let owner_index = capture.cohort.index(owner)?;
        let helper_index = capture.cohort.index(capture.helper.id())?;
        if unsafe {
            CompareObjectHandles(
                capture.cohort.processes[helper_index]
                    .handle
                    .as_raw_handle(),
                capture.helper.as_raw_handle(),
            )
        } == 0
        {
            return Err("packing helper identity mismatch".into());
        }
        let owner_image = capture.cohort.processes[owner_index].image_name()?;
        let mut holders = Vec::new();
        for (index, process) in capture.cohort.processes.iter().enumerate() {
            if process.pid != owner && process.image_name()?.eq_ignore_ascii_case(&owner_image) {
                holders.push(index);
            }
        }
        if holders.len() != 1 {
            return Err(format!(
                "expected one inert packing holder, found {}",
                holders.len()
            ));
        }
        let holder_index = holders[0];
        publish(
            &root,
            &format!("pack-{nonce}.bootstrap.release"),
            &bootstrap_release,
        )?;
        let (postpack, postpack_release) =
            capture_gate(&mut capture, &root, &nonce, "postpack", deadline)?;
        publish(
            &root,
            &format!("pack-{nonce}.postpack.release"),
            &postpack_release,
        )?;
        capture.cohort.processes[owner_index].wait(deadline)?;
        capture.cohort.processes[holder_index].wait(deadline)?;
        fs::write(root.join("helper-terminated"), "terminated")
            .map_err(|error| error.to_string())?;
        let final_ready = record(
            &root,
            &format!("pack-{nonce}.final.ready"),
            &mut capture.helper,
            deadline,
        )?;
        if identity(&final_ready, &nonce)? != capture.helper.id() {
            return Err("final gate helper mismatch".into());
        }
        let other = &final_ready["other"];
        let other_pid = other["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid != 0)
            .ok_or("missing joined other PID")?;
        if other["status"] != 0
            || !other["signal"].is_null()
            || !other["error"].is_null()
            || capture.cohort.processes.iter().any(|p| p.pid == other_pid)
        {
            return Err(
                "other install has no successful distinct synchronous completion receipt".into(),
            );
        }
        // In this empty fixture, install --path copies/publishes the executable;
        // no supervisor state exists, so it starts no children. spawnSync joins
        // that one process. No fixture or npm script spawns after this gate.
        capture.cohort.capture(&capture.job, 1, false)?;
        publish(
            &root,
            &format!("pack-{nonce}.final.release"),
            &json!({"nonce":nonce}),
        )?;
        let held = record(
            &root,
            &format!("pack-{nonce}.postpack.held"),
            &mut capture.helper,
            deadline,
        )?;
        let process = &capture.cohort.processes[postpack];
        if identity(&held, &nonce)? != process.pid || !process.live()? {
            return Err("postpack did not remain held".into());
        }
        wait_handle(capture.helper.as_raw_handle(), deadline)?;
        if !capture
            .helper
            .wait()
            .map_err(|error| error.to_string())?
            .success()
        {
            return Err("packing scenario failed".into());
        }
        if natural_release {
            publish(
                &root,
                &format!("pack-{nonce}.postpack.exit"),
                &json!({"nonce":nonce}),
            )?;
            process.wait(deadline)?;
            let mut exit = 0;
            if unsafe { GetExitCodeProcess(process.handle.as_raw_handle(), &mut exit) } == 0
                || exit != 0
            {
                return Err("natural postpack control did not exit successfully".into());
            }
        } else if !process.live()? {
            return Err("held postpack exited before termination".into());
        }
        capture.stopped = true;
        capture.cohort.settle(&capture.job)?;
        capture.settled = true;
        let count = capture.cohort.processes.len();
        capture.cohort.processes.clear(); // Close every added observer before ordinary retry.
        println!(
            "packing completion natural_release={natural_release}: {count} exact process handles signaled"
        );
        Ok(())
    })();
    let diagnostics = fs::read_to_string(&output_path).unwrap_or_else(|error| error.to_string());
    println!("{diagnostics}");
    assert!(
        result.is_ok(),
        "native packing completion failed: {result:?}"
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
            .filter_map(|entry| entry.ok())
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
    // Preserve the first observation; diagnostics must not turn a transient failure green.
    let (clear, first_remaining_stage) = no_stages();
    assert!(clear, "cleanup retained {first_remaining_stage:?}");
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
