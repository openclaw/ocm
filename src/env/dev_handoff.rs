use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::source_watch_session::SourceWatchSession;

#[cfg(windows)]
#[path = "dev_handoff_windows.rs"]
mod platform;

const REQUEST: &[u8] = b"handoff\n";
const ACK: &[u8] = b"\x06";
const MAX_REPLY: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) enum HandoffOutcome<T> {
    Link(T),
    Pending,
    Unavailable,
}

fn scope(session: &SourceWatchSession) -> String {
    let identity = serde_json::to_vec(&(&session.env_name, &session.env_root, &session.lease_id))
        .expect("the session identity is serializable");
    Sha256::digest(identity)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) struct HandoffServer {
    listener: platform::Listener,
}

pub(crate) struct HandoffRequest {
    stream: platform::Stream,
    deadline: Instant,
    disconnected: bool,
}

impl HandoffServer {
    pub(crate) fn bind(session: &SourceWatchSession) -> Result<Self, String> {
        Ok(Self {
            listener: platform::Listener::bind(&scope(session))?,
        })
    }

    pub(crate) fn poll(&mut self) -> Result<Option<HandoffRequest>, String> {
        let began = Instant::now();
        let Some(mut stream) = self.listener.accept()? else {
            return Ok(None);
        };
        // There is one fixed action. No command, source, destination, or credential
        // from the requester can change the controller's captured launch context.
        let mut request = [0_u8; REQUEST.len()];
        let deadline = began + Duration::from_millis(250);
        let mut received = 0;
        while received < request.len() {
            if Instant::now() >= deadline {
                return Ok(None);
            }
            match stream.read(&mut request[received..]) {
                Ok(0) => return Ok(None),
                Ok(count) => received += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return Ok(None),
            }
        }
        Ok((request == REQUEST).then_some(HandoffRequest {
            stream,
            deadline: began + REQUEST_TIMEOUT,
            disconnected: false,
        }))
    }

    pub(crate) fn close(&mut self) -> Result<(), String> {
        self.listener.close()
    }
}

impl HandoffRequest {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub(crate) fn disconnected(&mut self) -> bool {
        // A live requester keeps its write half open until receiving the reply.
        // Extra input also invalidates this single-action request.
        if !self.disconnected {
            self.disconnected = !matches!(self.stream.read(&mut [0_u8; 1]), Err(error) if error.kind() == io::ErrorKind::WouldBlock);
        }
        self.disconnected
    }

    pub(crate) fn pending(&mut self) {
        let _ = self.reply(&json!({ "pending": true }));
    }

    pub(crate) fn failed(&mut self, message: &str) {
        let _ = self.reply(&json!({ "error": message }));
    }

    pub(crate) fn deliver(&mut self, native: &[u8]) -> Result<(), String> {
        let value: Value = serde_json::from_slice(native)
            .map_err(|_| "the native dashboard did not return valid JSON".to_string())?;
        // The controller has already validated this handoff. Legacy shared-token
        // links, passwords, and unrelated native output never enter the reply.
        self.reply(&json!({ "handoff": {
            "ok": value.get("ok"),
            "browserUrl": value.get("browserUrl"),
            "browserBootstrapExpiresAtMs": value.get("browserBootstrapExpiresAtMs"),
        }}))
    }

    fn reply(&mut self, value: &Value) -> Result<(), String> {
        if self.expired() || self.disconnected() {
            return Err("the browser-link requester expired or disconnected".to_string());
        }
        let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        if bytes.len() >= MAX_REPLY {
            return Err("the native dashboard handoff is too large".to_string());
        }
        bytes.push(b'\n');
        let deadline = self.deadline.min(Instant::now() + Duration::from_secs(1));
        write_bounded(&mut self.stream, &bytes, deadline)
            .map_err(|error| format!("could not deliver the native dashboard handoff: {error}"))?;
        // Named-pipe closure can discard unread output. The one-byte ACK proves
        // the receiver consumed the complete frame without a blocking flush.
        loop {
            if Instant::now() >= deadline {
                return Err(
                    "the browser-link requester did not acknowledge the handoff".to_string()
                );
            }
            let mut ack = [0_u8; 1];
            match self.stream.read(&mut ack) {
                Ok(1) if ack == ACK && Instant::now() < deadline => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    return Err(
                        "the browser-link requester did not acknowledge the handoff".to_string()
                    );
                }
            }
        }
    }
}

pub(crate) fn request(
    session: &SourceWatchSession,
    stop: &AtomicBool,
) -> Result<HandoffOutcome<Vec<u8>>, String> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    if stop.load(Ordering::SeqCst) {
        return Err("dev dashboard request was cancelled".to_string());
    }
    let mut stream = match platform::connect(&scope(session), &session.controller) {
        Ok(stream) => stream,
        // Older controllers record the same UI session without a handoff endpoint.
        // The caller verifies that session and its processes before and after us.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HandoffOutcome::Unavailable);
        }
        Err(error)
            if error.kind() == io::ErrorKind::WouldBlock
                || (cfg!(target_os = "macos")
                    && error.kind() == io::ErrorKind::ConnectionRefused) =>
        {
            return Ok(HandoffOutcome::Pending);
        }
        Err(error) => {
            return Err(format!(
                "the dev UI owner could not accept a browser-link request: {error}"
            ));
        }
    };
    write_bounded(
        &mut stream,
        REQUEST,
        deadline.min(Instant::now() + Duration::from_secs(1)),
    )
    .map_err(|error| {
        format!("the dev UI owner could not receive a browser-link request: {error}")
    })?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err("dev dashboard request was cancelled".to_string());
        }
        if Instant::now() >= deadline {
            return Ok(HandoffOutcome::Pending);
        }
        match stream.read(&mut buffer) {
            Ok(0) => {
                return Err("the dev UI owner ended the browser-link request".to_string());
            }
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.len() > MAX_REPLY {
                    return Err(
                        "the dev UI owner returned an oversized browser-link reply".to_string()
                    );
                }
                if bytes.ends_with(b"\n") {
                    let reply: Value = serde_json::from_slice(&bytes).map_err(|_| {
                        "the dev UI owner returned an invalid browser-link reply".to_string()
                    })?;
                    let handoff = if let Some(error) = reply.get("error").and_then(Value::as_str) {
                        Err(error.to_string())
                    } else if reply.get("pending").and_then(Value::as_bool) == Some(true) {
                        Ok(HandoffOutcome::Pending)
                    } else {
                        reply
                            .get("handoff")
                            .ok_or_else(|| {
                                "the dev UI owner returned no browser handoff".to_string()
                            })
                            .and_then(|handoff| {
                                serde_json::to_vec(handoff)
                                    .map(HandoffOutcome::Link)
                                    .map_err(|error| error.to_string())
                            })
                    };
                    if stop.load(Ordering::SeqCst) {
                        return Err("dev dashboard request was cancelled".to_string());
                    }
                    write_bounded(
                        &mut stream,
                        ACK,
                        deadline.min(Instant::now() + Duration::from_secs(1)),
                    )
                    .map_err(|error| {
                        format!(
                            "the dev UI owner could not acknowledge the browser-link reply: {error}"
                        )
                    })?;
                    return handoff;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(error) => {
                return Err(format!(
                    "the dev UI owner could not deliver a browser link: {error}"
                ));
            }
        }
    }
}

pub(crate) fn cleanup_session(session: &SourceWatchSession) -> Result<(), String> {
    if session.ui.is_some()
        && session.process_scope == crate::infra::process_identity::process_scope_id()?
    {
        platform::cleanup(&scope(session))?;
    }
    Ok(())
}

fn write_bounded(stream: &mut platform::Stream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(error),
        }
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;
    use crate::infra::process_identity::{current_process_identity, process_scope_id};

    fn session(root: &std::path::Path) -> SourceWatchSession {
        serde_json::from_value(json!({
            "kind": "ocm-source-ui-session-v1",
            "envName": "handoff-fixture", "envRoot": root,
            "envCreatedAt": "2026-01-01T00:00:00Z", "leaseId": "handoff-fixture",
            "processScope": process_scope_id().unwrap(),
            "controller": current_process_identity().unwrap(),
            "watching": false, "ui": { "target": null, "children": {}, "pending": null },
            "childSpawnPending": false, "restoreService": false,
        }))
        .unwrap()
    }

    fn accept(server: &mut HandoffServer) -> HandoffRequest {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(request) = server.poll().unwrap() {
                return request;
            }
            assert!(
                Instant::now() < deadline,
                "the local handoff request did not arrive"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn dev_handoff_filters_native_credentials_and_confirms_reply_consumption() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path());
        let mut server = HandoffServer::bind(&session).unwrap();
        let client_session = session.clone();
        let client = std::thread::spawn(move || request(&client_session, &AtomicBool::new(false)));
        let mut pending = accept(&mut server);
        let native = json!({
            "ok": true, "browserUrl": "http://127.0.0.1:18789/#bootstrapToken=synthetic-owner-grant",
            "browserBootstrapExpiresAtMs": 2000000000000_i64,
            "url": "http://127.0.0.1:18789/#token=synthetic-shared-token",
            "gatewayPassword": "synthetic-shared-password",
        });
        pending
            .deliver(&serde_json::to_vec(&native).unwrap())
            .unwrap();
        let HandoffOutcome::Link(received) = client.join().unwrap().unwrap() else {
            panic!("expected the native browser handoff");
        };
        assert!(!String::from_utf8_lossy(&received).contains("synthetic-shared"));
        let received: Value = serde_json::from_slice(&received).unwrap();
        assert_eq!(received["browserUrl"], native["browserUrl"]);
        assert_eq!(received.as_object().unwrap().len(), 3);
        drop(pending);
        server.close().unwrap();
        cleanup_session(&session).unwrap();
        assert!(platform::connect(&scope(&session), &session.controller).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn dev_handoff_reports_saturated_admission_as_pending() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path());
        let mut server = HandoffServer::bind(&session).unwrap();
        let mut queued = Vec::new();
        let mut saturated = false;
        for _ in 0..8 {
            match platform::connect(&scope(&session), &session.controller) {
                Ok(stream) => queued.push(stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    saturated = true;
                    break;
                }
                Err(error) => panic!("could not fill the handoff queue: {error}"),
            }
        }
        assert!(saturated, "the bounded handoff queue did not fill");
        let began = Instant::now();
        assert!(matches!(
            request(&session, &AtomicBool::new(false)).unwrap(),
            HandoffOutcome::Pending
        ));
        assert!(began.elapsed() < Duration::from_secs(3));
        drop(queued);
        server.close().unwrap();
        cleanup_session(&session).unwrap();
    }

    #[test]
    fn dev_handoff_observes_requester_exit_and_rejects_another_controller() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path());
        let mut server = HandoffServer::bind(&session).unwrap();
        let mut client = platform::connect(&scope(&session), &session.controller).unwrap();
        write_bounded(
            &mut client,
            REQUEST,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        let mut pending = accept(&mut server);
        assert!(!pending.disconnected());
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !pending.disconnected() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(pending.disconnected());
        drop(pending);
        let mut foreign = session.controller.clone();
        foreign.pid = foreign.pid.saturating_add(1);
        assert!(platform::connect(&scope(&session), &foreign).is_err());
        let mut stale = session.controller.clone();
        stale.started_at = "different-process-start".to_string();
        assert!(platform::connect(&scope(&session), &stale).is_err());
        server.close().unwrap();
    }

    #[test]
    fn dev_handoff_bounds_framing_reply_and_ack_to_the_original_deadline() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path());
        let mut server = HandoffServer::bind(&session).unwrap();
        let mut client = platform::connect(&scope(&session), &session.controller).unwrap();
        client.write_all(b"hand").unwrap();
        let began = Instant::now();
        assert!(server.poll().unwrap().is_none());
        assert!(began.elapsed() < Duration::from_secs(2));
        drop(client);

        let mut client = platform::connect(&scope(&session), &session.controller).unwrap();
        client.write_all(REQUEST).unwrap();
        let mut pending = accept(&mut server);
        pending.deadline = Instant::now() + Duration::from_millis(50);
        let began = Instant::now();
        assert!(pending.reply(&json!({ "pending": true })).is_err());
        assert!(began.elapsed() < Duration::from_millis(500));
        let mut frame = [0_u8; 128];
        assert!(
            client.read(&mut frame).unwrap() > 0,
            "reply was never written"
        );
        assert!(pending.expired());
        assert!(pending.reply(&json!({ "pending": true })).is_err());
        assert_eq!(
            client.read(&mut frame).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(pending);
        drop(client);

        let mut client = platform::connect(&scope(&session), &session.controller).unwrap();
        client.write_all(REQUEST).unwrap();
        let mut pending = accept(&mut server);
        client.write_all(b"x").unwrap();
        assert!(pending.disconnected());
        assert!(pending.disconnected(), "invalid input must remain terminal");
        assert!(pending.reply(&json!({ "pending": true })).is_err());
        assert_eq!(
            client.read(&mut frame).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[cfg(unix)]
    #[test]
    fn dev_handoff_reclaims_an_abandoned_startup_endpoint_before_replacing_its_session() {
        use crate::env::source_watch_session::SourceWatchSessionPaths;
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let paths = SourceWatchSessionPaths::from_override(&root.path().join("watch.json"));
        crate::store::write_json(&paths.session, &session).unwrap();
        let mut server = HandoffServer::bind(&session).unwrap();
        assert!(
            paths
                .ensure_previous_session_finished(&session.env_name)
                .is_err()
        );
        assert!(platform::connect(&scope(&session), &session.controller).is_ok());
        // A controller can disappear after bind and before recording its first
        // child. Its retired birth identity must no longer keep that endpoint.
        let controller = session.controller.clone();
        session.controller.started_at = "retired-controller-start".to_string();
        paths.save_session(&session).unwrap();
        paths
            .ensure_previous_session_finished(&session.env_name)
            .unwrap();
        assert!(platform::connect(&scope(&session), &controller).is_err());
        server.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dev_handoff_bounds_socket_backpressure_and_preserves_a_rebound_listener() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path());
        let mut server = HandoffServer::bind(&session).unwrap();
        let mut clients = Vec::new();
        let began = Instant::now();
        for _ in 0..16 {
            match platform::connect(&scope(&session), &session.controller) {
                Ok(client) => clients.push(client),
                Err(_) => break,
            }
        }
        assert!(
            !clients.is_empty() && clients.len() < 16,
            "admission was not bounded"
        );
        assert!(began.elapsed() < Duration::from_secs(2));
        clients[0].write_all(REQUEST).unwrap();
        let mut pending = accept(&mut server);
        let began = Instant::now();
        let error = write_bounded(
            &mut pending.stream,
            &vec![b'x'; 2 * 1024 * 1024],
            began + Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(began.elapsed() < Duration::from_millis(500));
        drop(pending);
        server.close().unwrap();
        drop(clients);
        let mut replacement = HandoffServer::bind(&session).unwrap();
        drop(server);
        assert!(platform::connect(&scope(&session), &session.controller).is_ok());
        replacement.close().unwrap();
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::{self, DirBuilder};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use crate::infra::process_identity::ProcessIdentity;

    pub(crate) type Stream = UnixStream;

    pub(crate) struct Listener {
        socket: Option<UnixListener>,
        scope: String,
    }

    fn directory(scope: &str) -> PathBuf {
        // A fixed short parent keeps the endpoint within Darwin's sockaddr_un
        // limit even when OCM_HOME or TMPDIR is a deeply nested checkout path.
        PathBuf::from("/tmp").join(format!("ocm-dev-ui-{scope}"))
    }

    impl Listener {
        pub(crate) fn bind(scope: &str) -> Result<Self, String> {
            let root = directory(scope);
            DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .map_err(|error| {
                    format!("failed creating the private dev UI request directory: {error}")
                })?;
            let result = (|| {
                let socket = UnixListener::bind(root.join("socket"))?;
                fs::set_permissions(root.join("socket"), fs::Permissions::from_mode(0o600))?;
                socket.set_nonblocking(true)?;
                // Bound kernel admission as well as the controller's one active
                // request. A full queue fails the nonblocking connection attempt.
                if unsafe { libc::listen(socket.as_raw_fd(), 1) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok::<_, io::Error>(socket)
            })();
            match result {
                Ok(socket) => Ok(Self {
                    socket: Some(socket),
                    scope: scope.to_string(),
                }),
                Err(error) => {
                    let _ = cleanup(scope);
                    Err(format!(
                        "failed opening the private dev UI request socket: {error}"
                    ))
                }
            }
        }

        pub(crate) fn accept(&mut self) -> Result<Option<Stream>, String> {
            let Some(socket) = &self.socket else {
                return Ok(None);
            };
            match socket.accept() {
                Ok((stream, _)) => {
                    let Ok((_, uid)) = peer(&stream) else {
                        return Ok(None);
                    };
                    if uid != unsafe { libc::geteuid() } {
                        return Ok(None);
                    }
                    stream
                        .set_nonblocking(true)
                        .map_err(|error| error.to_string())?;
                    Ok(Some(stream))
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(error) => Err(format!(
                    "failed accepting a dev UI browser-link request: {error}"
                )),
            }
        }

        pub(crate) fn close(&mut self) -> Result<(), String> {
            if self.socket.take().is_some() {
                cleanup(&self.scope)?;
            }
            Ok(())
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }

    pub(crate) fn connect(scope: &str, controller: &ProcessIdentity) -> io::Result<Stream> {
        let endpoint = directory(scope).join("socket");
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1
        {
            return Err(io::Error::last_os_error());
        }
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as _;
        let bytes = endpoint.as_os_str().as_encoded_bytes();
        if bytes.len() >= address.sun_path.len() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
            *slot = *byte as _;
        }
        let size = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
        #[cfg(target_os = "macos")]
        {
            address.sun_len = size as _;
        }
        let result = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                std::ptr::from_ref(&address).cast(),
                size as _,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error);
            }
            let mut poll = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 250) } <= 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut pending = 0_i32;
            let mut size = std::mem::size_of_val(&pending) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    std::ptr::from_mut(&mut pending).cast(),
                    &mut size,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if pending != 0 {
                return Err(io::Error::from_raw_os_error(pending));
            }
        }
        let stream = UnixStream::from(fd);
        let (pid, uid) = peer(&stream)?;
        if uid != unsafe { libc::geteuid() } || pid != controller.pid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the local socket does not belong to the recorded dev UI controller",
            ));
        }
        if !crate::infra::process_identity::observe_process(pid)
            .map_err(io::Error::other)?
            .is_some_and(|process| process.running && process.identity == *controller)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the local socket controller's process identity changed",
            ));
        }
        Ok(stream)
    }

    pub(crate) fn cleanup(scope: &str) -> Result<(), String> {
        let root = directory(scope);
        let metadata = match fs::symlink_metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "failed inspecting the dev UI request directory: {error}"
                ));
            }
        };
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(
                "dev UI request directory ownership changed; its contents were preserved"
                    .to_string(),
            );
        }
        let socket = root.join("socket");
        match fs::symlink_metadata(&socket) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == unsafe { libc::geteuid() } =>
            {
                fs::remove_file(socket).map_err(|error| error.to_string())?;
            }
            Ok(_) => {
                return Err(
                    "the dev UI request socket was replaced; its contents were preserved"
                        .to_string(),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        fs::remove_dir(root)
            .map_err(|error| format!("failed removing the empty dev UI request directory: {error}"))
    }

    #[cfg(target_os = "linux")]
    fn peer(stream: &Stream) -> io::Result<(u32, libc::uid_t)> {
        let mut credential: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&credential) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::from_mut(&mut credential).cast(),
                &mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((credential.pid as u32, credential.uid))
    }

    #[cfg(target_os = "macos")]
    fn peer(stream: &Stream) -> io::Result<(u32, libc::uid_t)> {
        let mut uid = 0;
        let mut gid = 0;
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut pid: libc::pid_t = 0;
        let mut size = std::mem::size_of_val(&pid) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                std::ptr::from_mut(&mut pid).cast(),
                &mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((pid as u32, uid))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn peer(_: &Stream) -> io::Result<(u32, libc::uid_t)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "dev UI request ownership is unavailable on this platform",
        ))
    }
}
