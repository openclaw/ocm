#[cfg(target_os = "linux")]
use std::fs;
use std::io;
#[cfg(unix)]
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) started_at: String,
}

pub(crate) struct ProcessObservation {
    pub(crate) identity: ProcessIdentity,
    pub(crate) running: bool,
    pub(crate) stopped: bool,
    pub(crate) process_group: Option<u32>,
}

pub(crate) fn process_start_id(pid: u32) -> Result<Option<String>, String> {
    Ok(observe_process(pid)?.map(|process| process.identity.started_at))
}

pub(crate) fn current_process_identity() -> Result<ProcessIdentity, String> {
    observe_process(std::process::id())?
        .map(|process| process.identity)
        .ok_or_else(|| "failed to inspect the current process identity".to_string())
}

#[cfg(unix)]
pub(crate) fn process_group_members(group: u32) -> Result<Vec<ProcessObservation>, String> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid="])
        .output()
        .map_err(|error| format!("failed listing source watch process groups: {error}"))?;
    if !output.status.success() {
        return Err("failed listing source watch process groups".to_string());
    }
    let mut members = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(pgid)) = (fields.next(), fields.next()) else {
            continue;
        };
        if pgid.parse::<u32>().ok() != Some(group) {
            continue;
        }
        let pid = pid
            .parse::<u32>()
            .map_err(|error| format!("invalid source watch group member: {error}"))?;
        if let Some(process) = observe_process(pid)?
            && process.running
            && process.process_group == Some(group)
        {
            members.push(process);
        }
    }
    Ok(members)
}

#[cfg(target_os = "linux")]
pub(crate) fn process_scope_id() -> Result<Option<String>, String> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|error| format!("failed to inspect process boot identity: {error}"))?;
    let namespace = fs::read_link("/proc/self/ns/pid")
        .map_err(|error| format!("failed to inspect process namespace identity: {error}"))?;
    Ok(Some(format!("{}:{}", boot.trim(), namespace.display())))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn process_scope_id() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
pub(crate) fn observe_process(pid: u32) -> Result<Option<ProcessObservation>, String> {
    let path = format!("/proc/{pid}/stat");
    let stat = match fs::read_to_string(&path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to inspect process start identity for pid {pid}: {error}"
            ));
        }
    };
    let Some((_, fields)) = stat.rsplit_once(')') else {
        return Err(format!(
            "failed to parse process start identity for pid {pid}"
        ));
    };
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    let started_at = fields
        .get(19)
        .ok_or_else(|| format!("failed to parse process start identity for pid {pid}"))?;
    let process_group = fields
        .get(2)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| format!("failed to parse process group for pid {pid}"))?;
    Ok(Some(ProcessObservation {
        identity: ProcessIdentity {
            pid,
            started_at: (*started_at).to_string(),
        },
        running: !matches!(fields.first().copied(), Some("Z" | "X")),
        stopped: matches!(fields.first().copied(), Some("T" | "t")),
        process_group: Some(process_group),
    }))
}

#[cfg(target_os = "macos")]
pub(crate) fn observe_process(pid: u32) -> Result<Option<ProcessObservation>, String> {
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
    let size = std::mem::size_of_val(&info) as i32;
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            // Include unreaped exited processes so their recorded identity and
            // non-running status remain inspectable during group shutdown.
            1,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if bytes == 0
        && unsafe { libc::kill(pid as i32, 0) } == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return Ok(None);
    }
    if bytes != size {
        return Err(format!(
            "failed to inspect process start identity for pid {pid}"
        ));
    }
    if info.pbi_pid != pid {
        return Ok(None);
    }
    Ok(Some(ProcessObservation {
        identity: ProcessIdentity {
            pid,
            started_at: format!("{}:{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        },
        running: info.pbi_status != libc::SZOMB,
        stopped: info.pbi_status == libc::SSTOP,
        process_group: Some(info.pbi_pgid),
    }))
}

#[cfg(windows)]
pub(crate) fn observe_process(pid: u32) -> Result<Option<ProcessObservation>, String> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(None);
        }
        return Err(format!(
            "failed to inspect process start identity for pid {pid}: {error}"
        ));
    }
    let result = (|| {
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let mut exit_code = 0;
        if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
            || unsafe { GetExitCodeProcess(handle, &mut exit_code) } == 0
        {
            return Err(format!(
                "failed to inspect process start identity for pid {pid}: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Some(ProcessObservation {
            identity: ProcessIdentity {
                pid,
                started_at: (((created.dwHighDateTime as u64) << 32)
                    | created.dwLowDateTime as u64)
                    .to_string(),
            },
            running: exit_code == STILL_ACTIVE as u32,
            stopped: false,
            process_group: None,
        }))
    })();
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn observe_process(pid: u32) -> Result<Option<ProcessObservation>, String> {
    Err(format!(
        "process start identity inspection is unsupported for pid {pid} on this platform"
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::observe_process;
    use std::process::{Command, Stdio};

    #[test]
    fn observes_an_unreaped_exited_process() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                std::ptr::from_mut(&mut info),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        let observed = observe_process(pid);
        let reaped = child.wait();

        assert_eq!(waited, 0);
        assert!(reaped.unwrap().success());
        let observed = observed.unwrap().expect("unreaped process identity");
        assert_eq!(observed.identity.pid, pid);
        assert!(!observed.running);
        assert!(!observed.stopped);
    }
}
