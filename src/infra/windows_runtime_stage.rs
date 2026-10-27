//! Ephemeral Windows custody for children using a runtime installation stage.
//! The Job outlives the OCM parent while any launcher handle or member remains.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, OpenJobObjectW,
};
use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_ASSIGN_PROCESS, JOB_OBJECT_QUERY};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::file_identity::{FileIdentity, file_identity};

const JOB_ENV: &str = "OCM_INTERNAL_RUNTIME_STAGE_JOB";
const JOB_PREFIX: &str = "Global\\ocm-runtime-stage-";
const REQUEST_LIMIT: u64 = 1024 * 1024;

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn job_name(stage: &Path) -> io::Result<String> {
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(stage)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::other("runtime stage is not a directory"));
    }
    let FileIdentity::Windows {
        volume_serial_number,
        file_index,
    } = file_identity(&file, &metadata).map_err(io::Error::other)?;
    let mut digest = Sha256::new();
    digest.update(volume_serial_number.to_le_bytes());
    digest.update(file_index.to_le_bytes());
    // Stage basenames are generated from validated ASCII runtime names. Keep
    // aliases of the same physical directory in the same kernel namespace.
    for unit in stage
        .file_name()
        .ok_or_else(|| io::Error::other("runtime stage has no basename"))?
        .encode_wide()
    {
        let unit = if (b'A' as u16..=b'Z' as u16).contains(&unit) {
            unit + 32
        } else {
            unit
        };
        digest.update(unit.to_le_bytes());
    }
    let digest: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("{JOB_PREFIX}{digest}"))
}

pub(crate) struct StageJob {
    _handle: OwnedHandle,
    name: String,
}

impl StageJob {
    pub(crate) fn create(stage: &Path) -> io::Result<Self> {
        let name = job_name(stage)?;
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), wide(OsStr::new(&name)).as_ptr()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        if existed {
            return Err(io::Error::other("runtime stage already has child custody"));
        }
        // Default creator ACL, no breakaway and no kill-on-close limit. Ordinary
        // descendants retain membership even after this handle's owner exits.
        Ok(Self {
            _handle: handle,
            name,
        })
    }

    pub(crate) fn output(&self, command: &Command) -> io::Result<Output> {
        let request = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.encode_wide().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let request = serde_json::to_vec(&request).map_err(io::Error::other)?;
        if request.len() as u64 > REQUEST_LIMIT {
            return Err(io::Error::other(
                "runtime stage command exceeds its request bound",
            ));
        }
        let mut launcher = Command::new(std::env::current_exe()?);
        if let Some(cwd) = command.get_current_dir() {
            launcher.current_dir(cwd);
        }
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => launcher.env(key, value),
                None => launcher.env_remove(key),
            };
        }
        // These owner-built commands use null stdin and captured output. Keep
        // their argument bytes in a private pipe, not another command-line parser.
        launcher
            .env(JOB_ENV, &self.name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = launcher.spawn()?;
        let written = child.stdin.take().unwrap().write_all(&request);
        let output = child.wait_with_output()?;
        if output.status.success() {
            written?;
        }
        Ok(output)
    }
}

/// Absence is conclusive only after opening the exact Job reports not found.
/// An open launcher handle is custody even before it joins the Job itself.
pub(crate) fn has_custody(stage: &Path) -> io::Result<bool> {
    let name = match job_name(stage) {
        Ok(name) => name,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, wide(OsStr::new(&name)).as_ptr()) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
            Ok(false)
        } else {
            Err(error)
        };
    }
    drop(unsafe { OwnedHandle::from_raw_handle(raw) });
    Ok(true)
}

/// Same-executable private child dispatch; not a supported CLI entry point.
#[doc(hidden)]
pub fn try_run_internal_runtime_stage() -> Option<Result<i32, String>> {
    let name = std::env::var_os(JOB_ENV)?;
    Some(run_launcher(&name).map_err(|error| format!("runtime stage launcher: {error}")))
}

fn run_launcher(name: &OsStr) -> io::Result<i32> {
    name.to_str()
        .and_then(|name| name.strip_prefix(JOB_PREFIX))
        .filter(|name| name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| io::Error::other("invalid custody name"))?;
    let mut request = Vec::new();
    io::stdin()
        .take(REQUEST_LIMIT + 1)
        .read_to_end(&mut request)?;
    if request.len() as u64 > REQUEST_LIMIT {
        return Err(io::Error::other("command request exceeds its bound"));
    }
    let request: Vec<Vec<u16>> = serde_json::from_slice(&request).map_err(io::Error::other)?;
    let mut args = request.iter().map(|arg| OsString::from_wide(arg));
    let program = args
        .next()
        .filter(|program| !program.is_empty())
        .ok_or_else(|| io::Error::other("missing command program"))?;

    // Never create here. Parent death before this open must leave no writer;
    // after this open our handle bridges the interval before self-assignment.
    let raw = unsafe {
        OpenJobObjectW(
            JOB_OBJECT_ASSIGN_PROCESS | JOB_OBJECT_QUERY,
            0,
            wide(name).as_ptr(),
        )
    };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let status = Command::new(program)
        .args(args)
        .env_remove(JOB_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status.code().unwrap_or(1))
}
