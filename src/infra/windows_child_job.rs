//! Ephemeral command-tree custody. Runtime stages keep their separate durable Job contract.
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};

pub(crate) struct ChildJob {
    handle: OwnedHandle,
}

impl ChildJob {
    pub(crate) fn create() -> io::Result<Self> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    pub(crate) fn configure_command(&self, command: &mut Command) {
        // Admit the child to custody before its first instruction can create descendants.
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }

    pub(crate) fn assign_child(&self, child: &Child) -> io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), child.as_raw_handle()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn start_child(&self, child: &Child) -> io::Result<()> {
        resume_process(child.id())
    }

    pub(crate) fn stop_remaining(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.handle.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if unsafe {
                QueryInformationJobObject(
                    self.handle.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    std::ptr::from_mut(&mut info).cast(),
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if info.ActiveProcesses == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "Windows process job still has {} active processes",
                        info.ActiveProcesses
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

fn resume_process(pid: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
    while found {
        if entry.th32OwnerProcessID == pid {
            let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !raw.is_null() {
                let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
                return if unsafe { ResumeThread(thread.as_raw_handle()) } != u32::MAX {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                };
            }
        }
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Err(io::Error::other(format!(
        "failed resuming suspended process {pid}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    struct OwnedChild(Child);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn windows_child_job_gates_execution_until_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let job = ChildJob::create().unwrap();
        // Match the existing native Windows custody fixtures: real Node, no preloads.
        let mut command = Command::new("node");
        command
            .args([
                "-e",
                "require('node:fs').writeFileSync(process.argv[1], 'ran')",
                "--",
            ])
            .arg(&marker)
            .env_remove("NODE_OPTIONS")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        job.configure_command(&mut command);
        let mut child = OwnedChild(command.spawn().unwrap());
        thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists(), "child ran before Job admission");
        job.assign_child(&child.0).unwrap();
        assert!(!marker.exists(), "admission resumed the child");
        job.start_child(&child.0).unwrap();
        assert!(child.0.wait().unwrap().success());
        job.stop_remaining().unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "ran");
    }
}
