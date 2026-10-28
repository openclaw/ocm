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
        .map_err(|error| format!("failed listing process groups: {error}"))?;
    if !output.status.success() {
        return Err("failed listing process groups".to_string());
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
            .map_err(|error| format!("invalid process group member: {error}"))?;
        if let Some(process) = observe_process(pid)?
            && process.running
            && process.process_group == Some(group)
        {
            members.push(process);
        }
    }
    Ok(members)
}

/// Whether any member of `process_group` is still running. Zombies do not
/// count: their parent may never reap them.
#[cfg(unix)]
pub(crate) fn process_group_has_live_members(process_group: u32) -> Result<bool, String> {
    // PID 0 is never a process group member.
    process_group_has_live_members_except(process_group, 0)
}

/// Like `process_group_has_live_members`, ignoring the process `except`.
#[cfg(target_os = "linux")]
pub(crate) fn process_group_has_live_members_except(
    process_group: u32,
    except: u32,
) -> Result<bool, String> {
    for entry in fs::read_dir("/proc")
        .map_err(|error| format!("failed reading /proc for process group members: {error}"))?
    {
        let entry = entry.map_err(|error| {
            format!("failed reading /proc entry for process group members: {error}")
        })?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == except {
            continue;
        }
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            continue;
        };
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        let is_zombie = matches!(fields.first().copied(), Some("Z" | "X"));
        let in_group =
            fields.get(2).and_then(|value| value.parse::<u32>().ok()) == Some(process_group);
        if in_group && !is_zombie {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
pub(crate) fn process_group_has_live_members_except(
    process_group: u32,
    except: u32,
) -> Result<bool, String> {
    let capacity = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if capacity <= 0 {
        return Err(format!(
            "failed listing processes: {}",
            io::Error::last_os_error()
        ));
    }
    let mut pids = vec![0_i32; capacity as usize];
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if count < 0 {
        return Err(format!(
            "failed reading the process list: {}",
            io::Error::last_os_error()
        ));
    }
    for pid in pids
        .into_iter()
        .take(count as usize)
        .filter(|pid| *pid > 0 && *pid as u32 != except)
    {
        let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                std::mem::size_of_val(&info) as i32,
            )
        };
        if read == std::mem::size_of_val(&info) as i32
            && info.pbi_pgid == process_group
            && info.pbi_status != libc::SZOMB
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Without a process list, a group signal probe cannot exclude `except` and
/// counts zombies; callers must treat a positive answer as uncertain.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn process_group_has_live_members_except(
    process_group: u32,
    _except: u32,
) -> Result<bool, String> {
    if unsafe { libc::kill(-(process_group as libc::pid_t), 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(format!(
            "failed probing process group {process_group}: {error}"
        ))
    }
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

/// The parent process's executable path, then its short process name, for
/// whichever of them this platform can read. Unreadable values are omitted.
#[cfg(target_os = "linux")]
pub(crate) fn parent_process_names() -> Vec<String> {
    let parent = std::os::unix::process::parent_id();
    let mut names = Vec::new();
    if let Ok(path) = fs::read_link(format!("/proc/{parent}/exe")) {
        names.push(path.to_string_lossy().into_owned());
    }
    if let Ok(name) = fs::read_to_string(format!("/proc/{parent}/comm")) {
        names.push(name.trim_end_matches('\n').to_string());
    }
    names
}

#[cfg(target_os = "macos")]
pub(crate) fn parent_process_names() -> Vec<String> {
    fn until_nul(bytes: &[u8]) -> String {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    let parent = std::os::unix::process::parent_id() as i32;
    let mut names = Vec::new();
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe { libc::proc_pidpath(parent, path.as_mut_ptr().cast(), path.len() as u32) };
    if length > 0 {
        names.push(until_nul(&path[..(length as usize).min(path.len())]));
    }
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
    let size = std::mem::size_of_val(&info) as i32;
    let bytes = unsafe {
        libc::proc_pidinfo(
            parent,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if bytes == size {
        names.push(until_nul(&info.pbi_comm.map(|byte| byte as u8)));
    }
    names
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn parent_process_names() -> Vec<String> {
    Vec::new()
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
    parse_linux_process_stat(pid, &stat).map(Some)
}

#[cfg(target_os = "linux")]
fn parse_linux_process_stat(pid: u32, stat: &str) -> Result<ProcessObservation, String> {
    let Some((_, fields)) = stat.rsplit_once(')') else {
        return Err(format!(
            "failed to parse process start identity for pid {pid}"
        ));
    };
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    let started_at = fields
        .get(19)
        .ok_or_else(|| format!("failed to parse process start identity for pid {pid}"))?;
    // Linux can emit -1 after task release clears sighand. Its state was
    // sampled earlier, so that state cannot grant liveness or group authority.
    let process_group = match fields.get(2).copied() {
        Some("-1") => None,
        field => Some(
            field
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| format!("failed to parse process group for pid {pid}"))?,
        ),
    };
    Ok(ProcessObservation {
        identity: ProcessIdentity {
            pid,
            started_at: (*started_at).to_string(),
        },
        running: process_group.is_some() && !matches!(fields.first().copied(), Some("Z" | "X")),
        stopped: process_group.is_some() && matches!(fields.first().copied(), Some("T" | "t")),
        process_group,
    })
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

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::{ProcessIdentity, parse_linux_process_stat};

    // Captured from Ubuntu 6.8.0-147 while an owned child was reaped. State is
    // sampled before sighand locking, so the released group can follow live state.
    const RELEASED_STAT: &str = "39629 (ocm-exit-probe) X 0 -1 -1 0 -1 4227148 22 0 0 0 0 0 0 0 20 0 0 0 48761 0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 17 2 0 0 0 0 0 0 0 0 0 0 0 0 0\n";

    #[test]
    fn linux_stat_distinguishes_released_tasks_from_live_groups() {
        for state in ["X", "Z", "R", "S", "T", "t"] {
            let stat = RELEASED_STAT.replace(") X ", &format!(") {state} "));
            let observed = parse_linux_process_stat(39629, &stat).unwrap();
            assert_eq!(
                observed.identity,
                ProcessIdentity {
                    pid: 39629,
                    started_at: "48761".into()
                }
            );
            assert!(!observed.running, "{state}");
            assert!(!observed.stopped, "{state}");
            assert_eq!(observed.process_group, None, "{state}");
        }
        for (state, running, stopped) in
            [("R", true, false), ("T", true, true), ("Z", false, false)]
        {
            let stat = RELEASED_STAT.replace(") X 0 -1 ", &format!(") {state} 0 42 "));
            let observed = parse_linux_process_stat(39629, &stat).unwrap();
            assert_eq!(observed.process_group, Some(42));
            assert_eq!(observed.running, running);
            assert_eq!(observed.stopped, stopped);
        }
    }

    #[test]
    fn linux_stat_keeps_invalid_group_values_as_errors() {
        for group in ["-2", "-01", "4294967296", "invalid"] {
            let stat = RELEASED_STAT.replace(") X 0 -1 ", &format!(") X 0 {group} "));
            assert_eq!(
                parse_linux_process_stat(39629, &stat).err().as_deref(),
                Some("failed to parse process group for pid 39629")
            );
        }
    }
}
