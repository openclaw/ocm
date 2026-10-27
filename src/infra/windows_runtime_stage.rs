//! Stage-scoped Windows custody, independent of the embedding executable.
use super::file_identity::{FileIdentity, file_identity};
use super::windows_stage_command::PreparedCommand;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};
use std::thread;
use windows_sys::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ALREADY_EXISTS,
    ERROR_FILE_NOT_FOUND, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, OpenJobObjectW,
    SetInformationJobObject,
};
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_QUERY;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};
const JOB_PREFIX: &str = "Global\\ocm-runtime-stage-";
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
    handle: OwnedHandle,
}
impl StageJob {
    pub(crate) fn create(stage: &Path) -> io::Result<Self> {
        let name = job_name(stage)?;
        let encoded_name = wide(OsStr::new(&name));
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), encoded_name.as_ptr()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        if existed {
            return Err(io::Error::other("runtime stage already has child custody"));
        }
        // Default creator ACL; writers cannot break away and are never killed
        // when this handle closes. Children also retain a zero-access handle so
        // custody remains discoverable by name after the creator exits.
        Ok(Self { handle })
    }
    pub(crate) fn output(&self, command: &Command) -> io::Result<Output> {
        let mut command = PreparedCommand::from_stage_command(command)?;
        let stdin = fs::File::open("NUL")?;
        let (stdout_read, stdout_write) = io::pipe()?;
        let (stderr_read, stderr_write) = io::pipe()?;
        let stdout_reader = thread::Builder::new().spawn(move || read_all(stdout_read))?;
        let stderr_reader = match thread::Builder::new().spawn(move || read_all(stderr_read)) {
            Ok(reader) => reader,
            Err(error) => {
                drop(stdout_write);
                drop(stderr_write);
                let _ = stdout_reader.join();
                return Err(error);
            }
        };
        let spawned = (|| {
            let container = HandleContainer::create()?;
            let pin = pin_job(self.handle.as_raw_handle())?;
            let streams = container.duplicate(&[
                stdin.as_raw_handle(),
                stdout_write.as_raw_handle(),
                stderr_write.as_raw_handle(),
                pin.as_raw_handle(),
            ])?;
            let parent = container.process.as_raw_handle();
            let jobs = [self.handle.as_raw_handle()];
            let mut attributes = Attributes::new(3)?;
            attributes.set(
                PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
                std::slice::from_ref(&parent),
            )?;
            attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &streams.handles)?;
            attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
            let process = create_process(
                &mut command,
                &mut attributes,
                0,
                Some([streams.handles[0], streams.handles[1], streams.handles[2]]),
            )?;
            drop(attributes);
            // The inert parent must not hold a writer after creation.
            drop(streams);
            Ok::<_, io::Error>((process, container))
        })();
        drop(stdin);
        drop(stdout_write);
        drop(stderr_write);
        let (status, container) = match spawned {
            Ok((process, container)) => (wait_process(&process), Some(container)),
            Err(error) => (Err(error), None),
        };
        let stdout = join_reader(stdout_reader);
        let stderr = join_reader(stderr_reader);
        // Only the never-running holder is terminated. Actual writers belong
        // to the distinct stage Job and survive the creator's termination.
        drop(container);
        Ok(Output {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })
    }
}

// Process membership retains the Job object, but its name disappears when the
// last handle closes. Give only the intended child a lifetime pin, without Job
// query, mutation or termination rights. The embedding host's copy cannot leak
// through unrelated std::Command spawns because it is never inheritable here.
fn pin_job(job: HANDLE) -> io::Result<OwnedHandle> {
    let mut pin = std::ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            job,
            GetCurrentProcess(),
            &mut pin,
            0,
            0,
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(pin) })
}
fn read_all(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("runtime output reader panicked"))?
}

/// Microsoft's isolated handle-container pattern keeps inheritable handles out
/// of the embedding process, where unrelated legacy/std spawns could borrow them.
/// https://devblogs.microsoft.com/oldnewthing/20200306-00/?p=103538
struct HandleContainer {
    job: Option<OwnedHandle>,
    process: OwnedHandle,
}
impl HandleContainer {
    fn create() -> io::Result<Self> {
        let mut command =
            PreparedCommand::from_stage_command(&Command::new(std::env::current_exe()?))?;
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Child creation silently leaves only this private holder Job. Its
        // explicit JOB_LIST assigns the real child to the non-breakaway stage.
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let jobs = [job.as_raw_handle()];
        let mut attributes = Attributes::new(1)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
        // Creation and Job admission are atomic. This thread is never resumed;
        // owner death closes the private Job and cannot orphan a suspended holder.
        let process = create_process(&mut command, &mut attributes, CREATE_SUSPENDED, None)?;
        Ok(Self {
            job: Some(job),
            process,
        })
    }
    fn duplicate<'a>(&'a self, sources: &[HANDLE]) -> io::Result<RemoteHandles<'a>> {
        let mut copies = RemoteHandles {
            process: &self.process,
            handles: Vec::with_capacity(sources.len()),
        };
        for &source in sources {
            let mut copy = std::ptr::null_mut();
            if unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    source,
                    self.process.as_raw_handle(),
                    &mut copy,
                    0,
                    1,
                    DUPLICATE_SAME_ACCESS,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            copies.handles.push(copy);
        }
        Ok(copies)
    }
}
impl Drop for HandleContainer {
    fn drop(&mut self) {
        drop(self.job.take());
        // The sole private Job handle kills only this never-running process.
        let _ = unsafe { WaitForSingleObject(self.process.as_raw_handle(), INFINITE) };
    }
}
struct RemoteHandles<'a> {
    process: &'a OwnedHandle,
    handles: Vec<HANDLE>,
}
impl Drop for RemoteHandles<'_> {
    fn drop(&mut self) {
        for handle in self.handles.drain(..) {
            // CLOSE_SOURCE closes the remote source even on duplication error;
            // a terminated holder has already closed its handle table.
            unsafe {
                DuplicateHandle(
                    self.process.as_raw_handle(),
                    handle,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    DUPLICATE_CLOSE_SOURCE,
                );
            }
        }
    }
}
struct Attributes<'a> {
    storage: Vec<usize>,
    initialized: bool,
    values: PhantomData<&'a [u8]>,
}
impl<'a> Attributes<'a> {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut list = Self {
            storage: vec![0; bytes.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
            values: PhantomData,
        };
        if unsafe { InitializeProcThreadAttributeList(list.raw(), count, 0, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        list.initialized = true;
        Ok(list)
    }
    fn raw(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn set<T>(&mut self, key: u32, value: &'a [T]) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.raw(),
                0,
                key as usize,
                value.as_ptr().cast(),
                std::mem::size_of_val(value),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
impl Drop for Attributes<'_> {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.raw());
            }
        }
    }
}
fn create_process(
    command: &mut PreparedCommand,
    attributes: &mut Attributes<'_>,
    flags: u32,
    streams: Option<[HANDLE; 3]>,
) -> io::Result<OwnedHandle> {
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes.raw();
    if let Some([stdin, stdout, stderr]) = streams {
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin;
        startup.StartupInfo.hStdOutput = stdout;
        startup.StartupInfo.hStdError = stderr;
    }
    let mut process = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessW(
            command.application.as_ptr(),
            command.command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            i32::from(streams.is_some()),
            flags | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            command.environment.as_ptr().cast(),
            command
                .cwd
                .as_ref()
                .map_or(std::ptr::null(), |cwd| cwd.as_ptr()),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    drop(unsafe { OwnedHandle::from_raw_handle(process.hThread) });
    Ok(unsafe { OwnedHandle::from_raw_handle(process.hProcess) })
}
fn wait_process(process: &OwnedHandle) -> io::Result<ExitStatus> {
    if unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }
    let mut code = 0;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ExitStatus::from_raw(code))
}

/// Only absence of the exact named Job is conclusive; errors preserve custody.
pub(crate) fn has_custody(stage: &Path) -> io::Result<bool> {
    let name = match job_name(stage) {
        Ok(name) => name,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let encoded_name = wide(OsStr::new(&name));
    let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, encoded_name.as_ptr()) };
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

#[cfg(test)]
#[path = "windows_runtime_stage/tests.rs"]
mod tests;
