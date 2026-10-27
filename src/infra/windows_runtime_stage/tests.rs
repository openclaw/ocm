#![cfg(windows)]

use super::*;

use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{IsProcessInJob, TerminateJobObject};
use windows_sys::Win32::System::Threading::{
    GetProcessHandleCount, GetProcessId, OpenThread, SuspendThread, THREAD_SUSPEND_RESUME,
    TerminateProcess,
};

const DEADLINE: Duration = Duration::from_secs(15);

fn copy_handle(handle: HANDLE) -> OwnedHandle {
    let mut copy = std::ptr::null_mut();
    assert_ne!(
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle,
                GetCurrentProcess(),
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    unsafe { OwnedHandle::from_raw_handle(copy) }
}

fn noninheritable(handle: HANDLE) {
    let mut flags = 0;
    assert_ne!(
        unsafe { GetHandleInformation(handle, &mut flags) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0, "host handle is inheritable");
}

fn belongs(process: HANDLE, job: HANDLE) -> bool {
    let mut member = 0;
    assert_ne!(
        unsafe { IsProcessInJob(process, job, &mut member) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    member != 0
}

fn handle_count(process: HANDLE) -> u32 {
    let mut count = 0;
    assert_ne!(
        unsafe { GetProcessHandleCount(process, &mut count) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    count
}

// These guards can terminate only handles acquired from this test's creation
// calls. No PID lookup or operation on an installed environment is involved.
struct FixtureProcess(OwnedHandle);
impl FixtureProcess {
    fn wait(&self) {
        assert_eq!(
            unsafe { WaitForSingleObject(self.0.as_raw_handle(), 15_000) },
            WAIT_OBJECT_0,
            "fixture process did not exit"
        );
    }
}
impl Drop for FixtureProcess {
    fn drop(&mut self) {
        if unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } == WAIT_TIMEOUT {
            unsafe {
                TerminateProcess(self.0.as_raw_handle(), 99);
                WaitForSingleObject(self.0.as_raw_handle(), 5_000);
            }
        }
    }
}

struct FixtureChild(Child);
impl Drop for FixtureChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            unsafe {
                WaitForSingleObject(self.0.as_raw_handle(), 5_000);
            }
        }
    }
}

struct FixtureContainer {
    inner: Option<HandleContainer>,
    observer: OwnedHandle,
}
impl FixtureContainer {
    fn new() -> Self {
        let inner = HandleContainer::create().unwrap();
        let observer = copy_handle(inner.process.as_raw_handle());
        Self {
            inner: Some(inner),
            observer,
        }
    }
}
impl std::ops::Deref for FixtureContainer {
    type Target = HandleContainer;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}
impl Drop for FixtureContainer {
    fn drop(&mut self) {
        let container = self.inner.take().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            drop(container);
            let _ = sender.send(());
        });
        if receiver.recv_timeout(Duration::from_secs(5)).is_ok() {
            let _ = worker.join();
            return;
        }
        // Bound even a regression in production Drop. This is the exact
        // never-running holder process, not its nominal native child.
        unsafe {
            TerminateProcess(self.observer.as_raw_handle(), 99);
            WaitForSingleObject(self.observer.as_raw_handle(), 5_000);
        }
        if receiver.recv_timeout(Duration::from_secs(5)).is_ok() {
            let _ = worker.join();
        }
        if !thread::panicking() {
            panic!("holder Drop did not finish within its fixture deadline");
        }
    }
}

struct FixtureStage(OwnedHandle);
impl Drop for FixtureStage {
    fn drop(&mut self) {
        unsafe {
            TerminateJobObject(self.0.as_raw_handle(), 99);
        }
    }
}

fn bounded_output(job: StageJob, command: Command) -> io::Result<Output> {
    let cleanup = FixtureStage(copy_handle(job.handle.as_raw_handle()));
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let _ = sender.send(job.output(&command));
    });
    let output = receiver.recv_timeout(DEADLINE);
    drop(cleanup);
    match output {
        Ok(result) => {
            worker.join().unwrap();
            result
        }
        Err(error) => {
            // Unblock a regression that left a native writer running, then
            // report the original timeout. Never join an unbounded worker.
            if receiver.recv_timeout(Duration::from_secs(5)).is_ok() {
                worker.join().unwrap();
            }
            panic!("stage output did not complete: {error}");
        }
    }
}

fn node(script: &str) -> Command {
    let mut command = Command::new("node");
    command
        .args(["-e", script, "--"])
        .env_remove("NODE_OPTIONS");
    command
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + DEADLINE;
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "fixture did not create {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_in_container(
    command: &Command,
    container: &HandleContainer,
    stage: HANDLE,
    streams: &[HANDLE],
) -> io::Result<OwnedHandle> {
    let parent = container.process.as_raw_handle();
    let jobs = [stage];
    let mut prepared = PreparedCommand::from_stage_command(command)?;
    let mut attributes = Attributes::new(3)?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
        std::slice::from_ref(&parent),
    )?;
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, streams)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    create_process(
        &mut prepared,
        &mut attributes,
        0,
        Some([streams[0], streams[1], streams[2]]),
    )
}

#[test]
fn stage_output_preserves_native_arguments_environment_cwd_exit_and_large_streams() {
    let root = tempfile::tempdir().unwrap();
    let job = StageJob::create(root.path()).unwrap();
    let args = [
        "",
        "two words",
        "quote\"inside",
        "trailing\\",
        "%PATH%!&^",
        "λ",
    ];
    let mut command = node(
        r#"
const info = {args: process.argv.slice(1), cwd: process.cwd(),
  value: process.env.OCM_STAGE_COMMAND_TEST,
  pathRemoved: process.env.PATH === undefined,
  inputBytes: require('node:fs').readFileSync(0).length};
process.stdout.write(JSON.stringify(info) + '\n');
process.stdout.write(Buffer.alloc(128 * 1024, 83));
process.stderr.write(Buffer.alloc(128 * 1024, 69));
process.exitCode = 17;
"#,
    );
    command
        .args(args)
        .current_dir(root.path())
        .env("OCM_STAGE_COMMAND_TEST", "two words % ! λ")
        .env_remove("PATH")
        .env_remove("NODE_OPTIONS");
    let output = bounded_output(job, command).unwrap();
    assert_eq!(output.status.code(), Some(17));
    let newline = output
        .stdout
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&output.stdout[..newline]).unwrap();
    assert_eq!(info["args"], serde_json::json!(args));
    assert_eq!(info["value"], "two words % ! λ");
    assert_eq!(info["pathRemoved"], true);
    assert_eq!(info["inputBytes"], 0);
    assert_eq!(
        fs::canonicalize(info["cwd"].as_str().unwrap()).unwrap(),
        fs::canonicalize(root.path()).unwrap()
    );
    assert_eq!(&output.stdout[newline + 1..], vec![b'S'; 128 * 1024]);
    assert_eq!(output.stderr, vec![b'E'; 128 * 1024]);
}

#[test]
fn target_joins_stage_but_not_holder_job_and_holder_never_runs() {
    let root = tempfile::tempdir().unwrap();
    let stage = StageJob::create(root.path()).unwrap();
    let container = FixtureContainer::new();
    let observed_holder = copy_handle(container.process.as_raw_handle());
    let holder_job = container.job.as_ref().unwrap().as_raw_handle();
    assert!(belongs(container.process.as_raw_handle(), holder_job));
    assert!(!belongs(
        container.process.as_raw_handle(),
        stage.handle.as_raw_handle()
    ));

    // Query the initial suspension count without ever releasing the primary
    // thread. The extra suspension is immaterial: Drop terminates the holder.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    assert_ne!(
        snapshot,
        INVALID_HANDLE_VALUE,
        "{}",
        io::Error::last_os_error()
    );
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let pid = unsafe { GetProcessId(container.process.as_raw_handle()) };
    assert_ne!(pid, 0);
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut present = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
    let mut found = false;
    while present {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            assert!(!thread.is_null(), "{}", io::Error::last_os_error());
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            assert_eq!(
                unsafe { SuspendThread(thread.as_raw_handle()) },
                1,
                "holder primary thread was not parked at creation"
            );
            found = true;
            break;
        }
        present = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    assert!(found, "holder has no initial thread");

    let ready = root.path().join("ready");
    let release = root.path().join("release");
    let mut command = node(
        r#"
const fs = require('node:fs');
fs.writeFileSync(process.argv[1], 'ready');
const timer = setInterval(() => {
  if (fs.existsSync(process.argv[2])) { clearInterval(timer); process.exit(0); }
}, 20);
setTimeout(() => process.exit(98), 30000);
"#,
    );
    command.arg(&ready).arg(&release);
    let input = fs::File::open("NUL").unwrap();
    let output = fs::OpenOptions::new().write(true).open("NUL").unwrap();
    let streams = container
        .duplicate(&[
            input.as_raw_handle(),
            output.as_raw_handle(),
            output.as_raw_handle(),
        ])
        .unwrap();
    let target = FixtureProcess(
        spawn_in_container(
            &command,
            &container,
            stage.handle.as_raw_handle(),
            &streams.handles,
        )
        .unwrap(),
    );
    drop(streams);
    wait_for_file(&ready);
    assert!(belongs(
        target.0.as_raw_handle(),
        stage.handle.as_raw_handle()
    ));
    assert!(!belongs(target.0.as_raw_handle(), holder_job));
    drop(container);
    assert_eq!(
        unsafe { WaitForSingleObject(observed_holder.as_raw_handle(), 5_000) },
        WAIT_OBJECT_0
    );
    assert_eq!(
        unsafe { WaitForSingleObject(target.0.as_raw_handle(), 0) },
        WAIT_TIMEOUT,
        "holder cleanup terminated the real writer"
    );
    fs::write(release, b"done").unwrap();
    target.wait();
    assert!(wait_process(&target.0).unwrap().success());
}

#[test]
fn unrelated_std_child_cannot_hold_stage_capture_open() {
    let root = tempfile::tempdir().unwrap();
    let stage = StageJob::create(root.path()).unwrap();
    let container = FixtureContainer::new();
    let input = fs::File::open("NUL").unwrap();
    let error = fs::OpenOptions::new().write(true).open("NUL").unwrap();
    let (reader, writer) = io::pipe().unwrap();
    for handle in [
        input.as_raw_handle(),
        error.as_raw_handle(),
        reader.as_raw_handle(),
        writer.as_raw_handle(),
        container.process.as_raw_handle(),
        container.job.as_ref().unwrap().as_raw_handle(),
        stage.handle.as_raw_handle(),
    ] {
        noninheritable(handle);
    }
    let streams = container
        .duplicate(&[
            input.as_raw_handle(),
            writer.as_raw_handle(),
            error.as_raw_handle(),
        ])
        .unwrap();
    // Start an ordinary std caller (inherit-all on MSRV 1.88) while inheritable
    // copies exist in the holder. The handshake makes overlap deterministic.
    let ready = root.path().join("unrelated-ready");
    let release = root.path().join("unrelated-release");
    let mut unrelated = node(
        r#"
const fs = require('node:fs');
fs.writeFileSync(process.argv[1], 'ready');
setInterval(() => { if (fs.existsSync(process.argv[2])) process.exit(0); }, 20);
setTimeout(() => process.exit(98), 30000);
"#,
    );
    unrelated
        .arg(&ready)
        .arg(&release)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let unrelated = FixtureChild(unrelated.spawn().unwrap());
    wait_for_file(&ready);
    // Duplicating into the holder must not change the host source flags.
    noninheritable(writer.as_raw_handle());
    let target = FixtureProcess(
        spawn_in_container(
            &node("process.stdout.write('complete');"),
            &container,
            stage.handle.as_raw_handle(),
            &streams.handles,
        )
        .unwrap(),
    );
    drop(streams);
    drop(writer);
    let (sender, receiver) = mpsc::sync_channel(1);
    let read_worker = thread::spawn(move || {
        let _ = sender.send(read_all(reader));
    });
    target.wait();
    let captured = receiver.recv_timeout(Duration::from_secs(3));
    // Check before releasing the unrelated process: an EOF obtained only after
    // that process dies would conceal the inherited-writer regression.
    assert_eq!(
        unsafe { WaitForSingleObject(unrelated.0.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    match captured {
        Ok(output) => {
            assert_eq!(output.unwrap(), b"complete");
            read_worker.join().unwrap();
        }
        Err(error) => {
            drop(unrelated);
            drop(container);
            if receiver.recv_timeout(Duration::from_secs(5)).is_ok() {
                read_worker.join().unwrap();
            }
            panic!("unrelated std child retained a capture writer: {error}");
        }
    }
    fs::write(release, b"done").unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(unrelated.0.as_raw_handle(), 5_000) },
        WAIT_OBJECT_0
    );
}

#[test]
fn invalid_job_rejects_startup_and_partial_duplication_releases_remote_handles() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("must-not-run");
    let mut command = node("require('node:fs').writeFileSync(process.argv[1], 'ran');");
    command.arg(&marker);
    let container = FixtureContainer::new();
    let source = fs::File::open("NUL").unwrap();
    let output = fs::OpenOptions::new().write(true).open("NUL").unwrap();
    let streams = container
        .duplicate(&[
            source.as_raw_handle(),
            output.as_raw_handle(),
            output.as_raw_handle(),
        ])
        .unwrap();
    // NULL is invalid here. INVALID_HANDLE_VALUE is a current-process pseudo
    // handle in other APIs and must never be used as an invalid-owned-handle fixture.
    let rejected = spawn_in_container(&command, &container, std::ptr::null_mut(), &streams.handles);
    drop(streams);
    if let Ok(process) = rejected {
        let _cleanup = FixtureProcess(process);
        panic!("writer was admitted with a null Job handle");
    }
    assert!(
        !marker.exists(),
        "writer ran before Job admission succeeded"
    );

    let baseline = handle_count(container.process.as_raw_handle());
    {
        let copies = container
            .duplicate(&[source.as_raw_handle(), source.as_raw_handle()])
            .unwrap();
        assert_eq!(
            handle_count(container.process.as_raw_handle()),
            baseline + 2
        );
        drop(copies);
    }
    assert_eq!(handle_count(container.process.as_raw_handle()), baseline);
    assert!(
        container
            .duplicate(&[
                source.as_raw_handle(),
                source.as_raw_handle(),
                std::ptr::null_mut()
            ])
            .is_err()
    );
    assert_eq!(
        handle_count(container.process.as_raw_handle()),
        baseline,
        "partial DuplicateHandle failure leaked handles into the holder"
    );
    noninheritable(source.as_raw_handle());
}
