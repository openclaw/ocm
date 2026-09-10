use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::ptr;
use std::sync::{Arc, Weak};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING,
    ERROR_PIPE_NOT_CONNECTED, FILETIME, GENERIC_READ, GENERIC_WRITE, STILL_ACTIVE,
};
use windows_sys::Win32::Security::EqualSid;

use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, SetNamedPipeHandleState,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::infra::process_identity::ProcessIdentity;
use crate::infra::windows_security::{UserSid, owned_handle, with_user_only_security_attributes};

pub(super) struct Listener {
    pipe: Option<OwnedHandle>,
    user: UserSid,
    active: Weak<()>,
}

pub(super) struct Stream {
    handle: OwnedHandle,
    server: bool,
    // Release the active slot only after the server handle has been closed.
    _slot: Option<Arc<()>>,
}

impl Listener {
    pub(super) fn bind(scope: &str) -> Result<Self, String> {
        let name = pipe_name(scope).map_err(|error| error.to_string())?;
        let user = UserSid::current()
            .map_err(|error| format!("failed inspecting the dev UI pipe owner: {error}"))?;
        let pipe = create_pipe(&name, &user)
            .map_err(|error| format!("failed opening the private dev UI request pipe: {error}"))?;
        Ok(Self {
            pipe: Some(pipe),
            user,
            active: Weak::new(),
        })
    }

    pub(super) fn accept(&mut self) -> Result<Option<Stream>, String> {
        if self.active.upgrade().is_some() {
            return Ok(None);
        }
        let Some(pipe) = &self.pipe else {
            return Ok(None);
        };
        // In PIPE_NOWAIT mode, TRUE can mean only that this instance became
        // available. ERROR_PIPE_CONNECTED is the established connection state.
        if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), ptr::null_mut()) } != 0 {
            return Ok(None);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_PIPE_LISTENING) => return Ok(None),
            Some(ERROR_NO_DATA | ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => {
                disconnect(pipe).map_err(|error| error.to_string())?;
                return Ok(None);
            }
            Some(ERROR_PIPE_CONNECTED) => {}
            _ => {
                return Err(format!(
                    "failed accepting a dev UI browser-link request: {error}"
                ));
            }
        }
        if verify_peer(pipe, &self.user, None).is_err() {
            // A rejected or disappearing requester must not stop the dev session.
            disconnect(pipe).map_err(|error| error.to_string())?;
            return Ok(None);
        }
        // One instance refuses busy callers instead of queuing a later helper.
        // Retaining the original handle also keeps the name reserved between
        // requests. The non-inheritable duplicate refers to the same instance.
        let accepted = pipe
            .try_clone()
            .map_err(|error| format!("failed opening the dev UI request stream: {error}"))?;
        let slot = Arc::new(());
        self.active = Arc::downgrade(&slot);
        Ok(Some(Stream {
            handle: accepted,
            server: true,
            _slot: Some(slot),
        }))
    }

    pub(super) fn close(&mut self) -> Result<(), String> {
        self.pipe.take();
        Ok(())
    }
}

pub(super) fn connect(scope: &str, controller: &ProcessIdentity) -> io::Result<Stream> {
    let name = pipe_name(scope)?;
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            ptr::null_mut(),
        )
    };
    let handle = owned_handle(raw).map_err(|error| {
        if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
            io::Error::new(io::ErrorKind::WouldBlock, "the dev UI request pipe is busy")
        } else {
            error
        }
    })?;
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    if unsafe { SetNamedPipeHandleState(handle.as_raw_handle(), &mode, ptr::null(), ptr::null()) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = UserSid::current()?;
    verify_peer(&handle, &user, Some(controller))?;
    Ok(Stream {
        handle,
        server: false,
        _slot: None,
    })
}

pub(super) fn cleanup(_scope: &str) -> Result<(), String> {
    // No filesystem endpoint exists. Non-inheritable server handles close with
    // their controller, including when it crashes.
    Ok(())
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut read = 0;
        let result = unsafe {
            ReadFile(
                self.handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len().min(u32::MAX as usize) as u32,
                &mut read,
                ptr::null_mut(),
            )
        };
        if result != 0 {
            return Ok(read as usize);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_NO_DATA) => Err(io::ErrorKind::WouldBlock.into()),
            Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(0),
            _ => Err(error),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut written = 0;
        let result = unsafe {
            WriteFile(
                self.handle.as_raw_handle(),
                buffer.as_ptr(),
                buffer.len().min(u32::MAX as usize) as u32,
                &mut written,
                ptr::null_mut(),
            )
        };
        if result != 0 {
            return if written == 0 {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Ok(written as usize)
            };
        }
        Err(io::Error::last_os_error())
    }

    fn flush(&mut self) -> io::Result<()> {
        // The broker uses a bounded acknowledgement. FlushFileBuffers would
        // block the controller until the client consumed the reply.
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.server {
            // Successful replies have already been acknowledged by the client.
            // On failure, discard the abandoned connection without blocking.
            if disconnect(&self.handle).is_ok() {
                // The listener retains this instance. Re-arm it before releasing
                // the active slot so the next request needs no extra poll first.
                let _ = unsafe { ConnectNamedPipe(self.handle.as_raw_handle(), ptr::null_mut()) };
            }
        }
    }
}

fn pipe_name(scope: &str) -> io::Result<Vec<u16>> {
    if scope.len() != 64 || !scope.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid dev UI pipe scope",
        ));
    }
    Ok(format!(r"\\.\pipe\ocm-dev-ui-{scope}")
        .encode_utf16()
        .chain(Some(0))
        .collect())
}

fn create_pipe(name: &[u16], user: &UserSid) -> io::Result<OwnedHandle> {
    with_user_only_security_attributes(user, |attributes| {
        let raw = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                4096,
                0,
                attributes,
            )
        };
        owned_handle(raw)
    })
}

fn disconnect(pipe: &OwnedHandle) -> io::Result<()> {
    if unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED as i32) {
        Ok(())
    } else {
        Err(error)
    }
}

fn verify_peer(
    pipe: &OwnedHandle,
    user: &UserSid,
    controller: Option<&ProcessIdentity>,
) -> io::Result<()> {
    let mut pid = 0;
    let identified = unsafe {
        if controller.is_some() {
            GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid)
        } else {
            GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid)
        }
    };
    if identified == 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 || controller.is_some_and(|expected| expected.pid != pid) {
        return Err(unverified_peer());
    }
    let process = owned_handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
    if let Some(expected) = controller {
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut runtime = FILETIME::default();
        if unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut runtime,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let started = ((u64::from(created.dwHighDateTime) << 32)
            | u64::from(created.dwLowDateTime))
        .to_string();
        if started != expected.started_at {
            return Err(unverified_peer());
        }
    }
    let peer = UserSid::for_process(process.as_raw_handle())?;
    let mut exit_code = 0;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut exit_code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if exit_code != STILL_ACTIVE as u32 || unsafe { EqualSid(user.as_sid(), peer.as_sid()) } == 0 {
        return Err(unverified_peer());
    }
    Ok(())
}

fn unverified_peer() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "the local pipe peer does not match dev UI ownership",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::infra::process_identity::current_process_identity;

    fn scope() -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            ^ (u128::from(std::process::id()) << 96)
            ^ (u128::from(NEXT.fetch_add(1, Ordering::Relaxed)) << 64);
        format!("{id:064x}")
    }

    fn accept(listener: &mut Listener) -> Stream {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(stream) = listener.accept().unwrap() {
                return stream;
            }
            assert!(
                Instant::now() < deadline,
                "pipe did not accept its verified client"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn windows_handoff_pipe_round_trip_and_single_active_request() {
        let scope = scope();
        let mut listener = Listener::bind(&scope).unwrap();
        assert!(Listener::bind(&scope).is_err());
        let identity = current_process_identity().unwrap();
        let mut client = connect(&scope, &identity).unwrap();
        let mut server = accept(&mut listener);
        assert_eq!(
            server.read(&mut [0_u8; 8]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        client.write_all(b"handoff\n").unwrap();
        let mut request = [0_u8; 8];
        server.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"handoff\n");
        assert!(listener.accept().unwrap().is_none());
        assert!(
            matches!(connect(&scope, &identity), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        server.write_all(b"reply\n").unwrap();
        let mut reply = [0_u8; 6];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"reply\n");
        drop(server);
        // Reuse the retained instance immediately, even while the former client
        // holds its disconnected handle. No name can be claimed in between.
        assert!(Listener::bind(&scope).is_err());
        let mut next = connect(&scope, &identity).unwrap();
        let mut server = accept(&mut listener);
        next.write_all(b"handoff\n").unwrap();
        server.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"handoff\n");
        assert_eq!(client.read(&mut [0_u8; 1]).unwrap(), 0);
        drop(client);
        drop(next);
        assert_eq!(server.read(&mut [0_u8; 1]).unwrap(), 0);
        drop(server);
        listener.close().unwrap();
        assert!(connect(&scope, &identity).is_err());
    }

    #[test]
    fn windows_handoff_pipe_rejects_a_stale_controller_start_identity() {
        let scope = scope();
        let _listener = Listener::bind(&scope).unwrap();
        let mut identity = current_process_identity().unwrap();
        identity.started_at = "different-process-start".to_string();
        assert!(
            matches!(connect(&scope, &identity), Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
        );
    }
}
