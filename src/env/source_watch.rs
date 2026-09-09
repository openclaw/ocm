use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::EnvironmentService;
use crate::store::{
    ExclusiveFileLock, display_path, ensure_dir, lock_file, now_utc, read_json,
    source_watch_override_path, try_lock_file, validate_name, write_json,
};

const SOURCE_WATCH_OVERRIDE_KIND: &str = "ocm-source-watch-override";
const SOURCE_WATCH_LOCK_RETRY_ATTEMPTS: usize = 20;
const SOURCE_WATCH_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceWatchOverride {
    pub kind: String,
    pub env_name: String,
    pub repo_root: String,
    pub watch_pid: u32,
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub(crate) struct CreateSourceWatchOverrideOptions {
    pub(crate) env_name: String,
    pub(crate) repo_root: PathBuf,
    pub(crate) watch_pid: u32,
}

#[derive(Debug)]
pub(crate) struct SourceWatchLease {
    env_name: String,
    lease_id: String,
    lock_file: File,
    service_was_running: bool,
    #[cfg(windows)]
    lease_event: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for SourceWatchLease {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.lease_event);
        }
    }
}

impl SourceWatchLease {
    pub(crate) fn service_was_running(&self) -> bool {
        self.service_was_running
    }

    pub(crate) fn begin_service_restore(&mut self) -> Result<(), String> {
        write_source_watch_lock(&mut self.lock_file, &format!("restoring:{}", self.lease_id))
    }

    #[cfg(unix)]
    pub(crate) fn configure_child(&self, command: &mut Command) {
        let lock_fd = self.lock_file.as_raw_fd();
        command.process_group(0);
        // The watcher inherits the lease so an OCM crash cannot release
        // exclusivity while the source gateway remains alive.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(lock_fd, libc::F_GETFD);
                if flags == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fcntl(lock_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn configure_child(&self, _command: &mut Command) {}

    #[cfg(windows)]
    pub(crate) fn attach_to_child(&self, child: &std::process::Child) -> Result<(), String> {
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let mut child_handle: HANDLE = std::ptr::null_mut();
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.lease_event,
                child.as_raw_handle() as HANDLE,
                &mut child_handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(format!(
                "failed attaching source watch lease to child: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    pub(crate) fn attach_to_child(&self, _child: &std::process::Child) -> Result<(), String> {
        Ok(())
    }
}

impl SourceWatchOverride {
    pub fn openclaw_entry_path(&self) -> PathBuf {
        Path::new(&self.repo_root).join("openclaw.mjs")
    }

    pub fn extensions_dir(&self) -> PathBuf {
        Path::new(&self.repo_root).join("extensions")
    }

    pub fn command_label(&self) -> String {
        format!("node {}", display_path(&self.openclaw_entry_path()))
    }
}

impl<'a> EnvironmentService<'a> {
    pub(crate) fn lock_gateway_admission(
        &self,
        env_name: &str,
    ) -> Result<ExclusiveFileLock, String> {
        lock_file(&self.gateway_admission_path(env_name)?, "gateway admission")
    }

    pub(crate) fn try_lock_gateway_admission(
        &self,
        env_name: &str,
    ) -> Result<Option<ExclusiveFileLock>, String> {
        try_lock_file(&self.gateway_admission_path(env_name)?, "gateway admission")
    }

    fn gateway_admission_path(&self, env_name: &str) -> Result<PathBuf, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        Ok(source_watch_override_path(&env_name, self.env, self.cwd)?
            .with_extension("admission.lock"))
    }

    pub(crate) fn ensure_source_watch_allows_service(&self, env_name: &str) -> Result<(), String> {
        if self.active_source_watch_override(env_name)?.is_some() {
            return Err(format!(
                "background service for env \"{env_name}\" cannot start while source watch is active; stop the watch session first"
            ));
        }
        Ok(())
    }

    pub(crate) fn acquire_source_watch_lease(
        &self,
        env_name: &str,
        allow_service_takeover: bool,
    ) -> Result<SourceWatchLease, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        // Commands keep the operation lock while changing service policy; the
        // shorter admission lock also covers starts from an already planned daemon.
        let _operation_lock = self.lock_operation(&env_name)?;
        let _admission_lock = self.lock_gateway_admission(&env_name)?;
        let meta = self.get(&env_name)?;
        if meta.service_running && !allow_service_takeover {
            return Err(format!(
                "dev env {env_name} is already running in the background; stop it first or rerun with --watch --force to take it over temporarily"
            ));
        }
        let override_path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        let lock_path = override_path.with_extension("lock");
        if let Ok(meta) = read_json::<SourceWatchOverride>(&override_path)
            && !is_leased_source_watch(&meta)
            && is_valid_source_watch_metadata(&meta, &env_name)
            && is_legacy_source_watch_process(&meta)
        {
            return Err(format!(
                "source watch for env \"{env_name}\" is already active with legacy pid {}",
                meta.watch_pid
            ));
        }
        if let Some(parent) = lock_path.parent() {
            ensure_dir(parent)?;
        }
        let mut lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                format!(
                    "failed opening source watch lock {}: {error}",
                    display_path(&lock_path)
                )
            })?;
        #[cfg(windows)]
        let lease_event = acquire_windows_source_watch_event(&lock_path, &env_name)?;
        match try_lock_source_watch_exclusive(&lock_file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                #[cfg(windows)]
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(lease_event);
                }
                return Err(format!(
                    "source watch for env \"{env_name}\" is already active or starting"
                ));
            }
            Err(error) => {
                #[cfg(windows)]
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(lease_event);
                }
                return Err(format!(
                    "failed locking source watch for env \"{env_name}\": {error}"
                ));
            }
        }

        let lease_id = format!(
            "{}-{}",
            std::process::id(),
            now_utc().unix_timestamp_nanos()
        );
        // Publish the new generation before touching stale metadata. Readers that
        // observe this exclusive owner must never correlate an old token with it.
        write_source_watch_lock(&mut lock_file, &lease_id)?;

        // Owning the OS lock proves any surviving metadata belongs to a dead
        // lease, even if its child PID has since been reused by another process.
        remove_file_if_present(&override_path)?;
        Ok(SourceWatchLease {
            env_name,
            lease_id,
            lock_file,
            service_was_running: meta.service_running,
            #[cfg(windows)]
            lease_event,
        })
    }

    pub(crate) fn create_source_watch_override_with_lease(
        &self,
        options: CreateSourceWatchOverrideOptions,
        lease: &SourceWatchLease,
    ) -> Result<SourceWatchOverride, String> {
        if options.env_name != lease.env_name {
            return Err(format!(
                "source watch lease for env \"{}\" cannot create an override for env \"{}\"",
                lease.env_name, options.env_name
            ));
        }
        self.write_source_watch_override(options, &lease.lease_id)
    }

    fn write_source_watch_override(
        &self,
        options: CreateSourceWatchOverrideOptions,
        lease_id: &str,
    ) -> Result<SourceWatchOverride, String> {
        let env_name = validate_name(&options.env_name, "Environment name")?;
        let path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        let token = format!(
            "lease:{lease_id}:{}-{}",
            options.watch_pid,
            now_utc().unix_timestamp_nanos()
        );
        let meta = SourceWatchOverride {
            kind: SOURCE_WATCH_OVERRIDE_KIND.to_string(),
            env_name,
            repo_root: display_path(&options.repo_root),
            watch_pid: options.watch_pid,
            token,
            started_at: now_utc(),
        };
        write_json(&path, &meta)?;
        Ok(meta)
    }

    pub fn clear_source_watch_override(&self, env_name: &str, token: &str) -> Result<bool, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        let path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        if !path.exists() {
            return Ok(false);
        }
        let existing = match read_json::<SourceWatchOverride>(&path) {
            Ok(existing) => existing,
            Err(_) => {
                remove_file_if_present(&path)?;
                return Ok(true);
            }
        };
        if existing.token != token {
            return Ok(false);
        }
        remove_file_if_present(&path)?;
        Ok(true)
    }

    pub fn active_source_watch_override(
        &self,
        env_name: &str,
    ) -> Result<Option<SourceWatchOverride>, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        let path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        let lock_path = path.with_extension("lock");
        let lock_file = match OpenOptions::new().read(true).open(&lock_path) {
            Ok(lock_file) => lock_file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A lookup with no watch state remains read-only. If an override exists,
                // create the lock before cleanup so lease creation cannot interleave.
                if !path.exists() {
                    return Ok(None);
                }
                if let Some(parent) = lock_path.parent() {
                    ensure_dir(parent)?;
                }
                open_source_watch_lock(&lock_path)?
            }
            Err(error) => {
                return Err(format!(
                    "failed opening source watch lock {}: {error}",
                    display_path(&lock_path)
                ));
            }
        };

        #[cfg(windows)]
        if let Some(_lease_event) = open_windows_source_watch_event(&lock_path)? {
            let lock_lease_id = fs::read_to_string(&lock_path)
                .map_err(|error| {
                    format!(
                        "failed reading active source watch lock {}: {error}",
                        display_path(&lock_path)
                    )
                })?
                .trim()
                .to_string();
            if source_watch_lock_is_restoring(&lock_lease_id) {
                remove_file_if_present(&path)?;
                return Ok(None);
            }
            let meta = read_json::<SourceWatchOverride>(&path).map_err(|error| {
                format!(
                    "source watch for env \"{env_name}\" is active or starting, but its metadata is unavailable: {error}"
                )
            })?;
            if source_watch_matches_lease(&meta, &lock_lease_id)
                && is_valid_source_watch_structure(&meta, &env_name)
            {
                return Ok(Some(meta));
            }
            return Err(format!(
                "source watch for env \"{env_name}\" is active or starting, but its metadata does not match the active lease"
            ));
        }

        match FileExt::try_lock_shared(&lock_file) {
            Ok(()) => {
                // Shared readers prove no watcher owns the exclusive lease. They may clean the
                // same stale metadata concurrently without impersonating an active watcher.
                let active_legacy = read_json::<SourceWatchOverride>(&path).ok().filter(|meta| {
                    !is_leased_source_watch(meta)
                        && is_valid_source_watch_metadata(meta, &env_name)
                        && is_legacy_source_watch_process(meta)
                });
                if active_legacy.is_none() {
                    remove_file_if_present(&path)?;
                }
                FileExt::unlock(&lock_file).map_err(|error| {
                    format!(
                        "failed unlocking stale source watch lock {}: {error}",
                        display_path(&lock_path)
                    )
                })?;
                Ok(active_legacy)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let lock_lease_id = fs::read_to_string(&lock_path)
                    .map_err(|error| {
                        format!(
                            "failed reading active source watch lock {}: {error}",
                            display_path(&lock_path)
                        )
                    })?
                    .trim()
                    .to_string();
                if source_watch_lock_is_restoring(&lock_lease_id) {
                    remove_file_if_present(&path)?;
                    return Ok(None);
                }
                let meta = read_json::<SourceWatchOverride>(&path).map_err(|error| {
                    format!(
                        "source watch for env \"{env_name}\" is active or starting, but its metadata is unavailable: {error}"
                    )
                })?;
                let metadata_valid = source_watch_matches_lease(&meta, &lock_lease_id)
                    && is_valid_source_watch_structure(&meta, &env_name);
                if metadata_valid {
                    Ok(Some(meta))
                } else {
                    Err(format!(
                        "source watch for env \"{env_name}\" is active or starting, but its metadata does not match the active lease"
                    ))
                }
            }
            Err(error) => Err(format!(
                "failed checking source watch lock {}: {error}",
                display_path(&lock_path)
            )),
        }
    }
}

fn write_source_watch_lock(lock_file: &mut File, value: &str) -> Result<(), String> {
    lock_file
        .set_len(0)
        .and_then(|()| lock_file.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| writeln!(lock_file, "{value}"))
        .map_err(|error| format!("failed recording source watch lock: {error}"))
}

fn try_lock_source_watch_exclusive(lock_file: &File) -> io::Result<()> {
    for attempt in 0..=SOURCE_WATCH_LOCK_RETRY_ATTEMPTS {
        match FileExt::try_lock_exclusive(lock_file) {
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && attempt < SOURCE_WATCH_LOCK_RETRY_ATTEMPTS =>
            {
                thread::sleep(SOURCE_WATCH_LOCK_RETRY_DELAY);
            }
            result => return result,
        }
    }
    unreachable!("bounded source watch lock retry must return")
}

fn source_watch_lock_is_restoring(lock_value: &str) -> bool {
    lock_value.starts_with("restoring:")
}

fn is_leased_source_watch(meta: &SourceWatchOverride) -> bool {
    meta.token.starts_with("lease:")
}

fn source_watch_matches_lease(meta: &SourceWatchOverride, lease_id: &str) -> bool {
    if lease_id.is_empty() {
        return !is_leased_source_watch(meta) && is_process_alive(meta.watch_pid);
    }
    meta.token
        .strip_prefix("lease:")
        .and_then(|token| token.split_once(':'))
        .is_some_and(|(metadata_lease_id, _)| metadata_lease_id == lease_id)
}

fn is_valid_source_watch_metadata(meta: &SourceWatchOverride, env_name: &str) -> bool {
    is_valid_source_watch_structure(meta, env_name) && is_process_alive(meta.watch_pid)
}

fn is_valid_source_watch_structure(meta: &SourceWatchOverride, env_name: &str) -> bool {
    meta.kind == SOURCE_WATCH_OVERRIDE_KIND
        && meta.env_name == env_name
        && !meta.repo_root.trim().is_empty()
        && !meta.token.trim().is_empty()
        && meta.watch_pid > 0
        && Path::new(&meta.repo_root).join("openclaw.mjs").is_file()
        && Path::new(&meta.repo_root).join("extensions").is_dir()
}

#[cfg(windows)]
struct WindowsSourceWatchEvent {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for WindowsSourceWatchEvent {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn acquire_windows_source_watch_event(
    lock_path: &Path,
    env_name: &str,
) -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateEventW;

    let event_name = windows_source_watch_event_name(lock_path);
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, event_name.as_ptr()) };
    if event.is_null() {
        return Err(format!(
            "failed creating source watch lease for env \"{env_name}\": {}",
            io::Error::last_os_error()
        ));
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(event);
        }
        return Err(format!(
            "source watch for env \"{env_name}\" is already active or starting"
        ));
    }
    Ok(event)
}

#[cfg(windows)]
fn open_windows_source_watch_event(
    lock_path: &Path,
) -> Result<Option<WindowsSourceWatchEvent>, String> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, GetLastError};
    use windows_sys::Win32::System::Threading::{OpenEventW, SYNCHRONIZATION_SYNCHRONIZE};

    let event_name = windows_source_watch_event_name(lock_path);
    let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, event_name.as_ptr()) };
    if !event.is_null() {
        return Ok(Some(WindowsSourceWatchEvent { handle: event }));
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_FILE_NOT_FOUND {
        Ok(None)
    } else {
        Err(format!(
            "failed checking source watch lease {}: {}",
            display_path(lock_path),
            io::Error::from_raw_os_error(error as i32)
        ))
    }
}

#[cfg(windows)]
fn windows_source_watch_event_name(lock_path: &Path) -> Vec<u16> {
    let digest = Sha256::digest(display_path(lock_path).as_bytes());
    let suffix = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("Local\\OCM-source-watch-{suffix}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn open_source_watch_lock(lock_path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| {
            format!(
                "failed opening source watch lock {}: {error}",
                display_path(lock_path)
            )
        })
}

fn is_legacy_source_watch_process(meta: &SourceWatchOverride) -> bool {
    let command_matches = process_command_line(meta.watch_pid)
        .is_some_and(|command| legacy_source_watch_command_matches(&command));
    if !command_matches {
        return false;
    }
    legacy_source_watch_cwd_matches(meta)
}

fn legacy_source_watch_command_matches(command: &str) -> bool {
    let command = command.replace('\\', "/");
    command.contains("node")
        && command.contains("scripts/watch-node.mjs")
        && command.contains("gateway")
        && command.contains("run")
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed removing {}: {error}", display_path(path))),
    }
}

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_command_line(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "linux")]
fn process_command_line(pid: u32) -> Option<String> {
    let command = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let command = command
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
}

#[cfg(target_os = "linux")]
fn legacy_source_watch_cwd_matches(meta: &SourceWatchOverride) -> bool {
    fs::read_link(format!("/proc/{}/cwd", meta.watch_pid))
        .ok()
        .is_some_and(|cwd| same_path(&cwd, Path::new(&meta.repo_root)))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn legacy_source_watch_cwd_matches(meta: &SourceWatchOverride) -> bool {
    let output = Command::new("lsof")
        .args([
            "-b",
            "-w",
            "-a",
            "-p",
            &meta.watch_pid.to_string(),
            "-d",
            "cwd",
            "-Fn",
        ])
        .output();
    let Ok(output) = output else {
        return false;
    };
    output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix('n'))
            .is_some_and(|cwd| same_path(Path::new(cwd), Path::new(&meta.repo_root)))
}

#[cfg(windows)]
fn legacy_source_watch_cwd_matches(_meta: &SourceWatchOverride) -> bool {
    true
}

#[cfg(unix)]
fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    Command::new("tasklist")
        .args(["/FI", &filter])
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_command_line(pid: u32) -> Option<String> {
    let script = format!("(Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}').CommandLine");
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_watch_override_labels_the_built_entry() {
        let meta = SourceWatchOverride {
            kind: SOURCE_WATCH_OVERRIDE_KIND.to_string(),
            env_name: "demo".to_string(),
            repo_root: "/repo/openclaw".to_string(),
            watch_pid: 123,
            token: "123-token".to_string(),
            started_at: OffsetDateTime::UNIX_EPOCH,
        };

        assert_eq!(
            meta.openclaw_entry_path(),
            PathBuf::from("/repo/openclaw/openclaw.mjs")
        );
        assert_eq!(
            meta.extensions_dir(),
            PathBuf::from("/repo/openclaw/extensions")
        );
        assert_eq!(meta.command_label(), "node /repo/openclaw/openclaw.mjs");
    }

    #[test]
    fn legacy_source_watch_identity_requires_the_watch_command() {
        assert!(legacy_source_watch_command_matches(
            "node scripts/watch-node.mjs gateway run --port 18789"
        ));
        assert!(!legacy_source_watch_command_matches(
            "cargo test source_watch"
        ));
    }

    #[test]
    fn exclusive_source_watch_lock_retries_transient_shared_readers() {
        let path = std::env::temp_dir().join(format!(
            "ocm-source-watch-lock-retry-{}-{}",
            std::process::id(),
            now_utc().unix_timestamp_nanos()
        ));
        let reader = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        FileExt::lock_shared(&reader).unwrap();
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release_flag = std::sync::Arc::clone(&released);
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            FileExt::unlock(&reader).unwrap();
            release_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        try_lock_source_watch_exclusive(&contender).unwrap();

        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
        FileExt::unlock(&contender).unwrap();
        release.join().unwrap();
        fs::remove_file(path).unwrap();
    }
}
