//! Detect namespace replacement while SQLite uses its pathname-based VFS.
//!
//! The archive walker reads pinned descriptors, whereas SQLite opens the database
//! and its sidecars by name. Watch the selected ancestors before opening SQLite,
//! and discard its private snapshot if any of those names change, including an
//! ancestor moved away and back. Ordinary database writes are not watched.

use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

pub(super) struct SqlitePathGuard {
    events: File,
    // Keep watched objects alive, even after unlink, until events are checked.
    _files: Vec<File>,
    path: PathBuf,
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "SQLite source path changed during capture",
    )
}

impl SqlitePathGuard {
    pub(super) fn new(
        root: &Path,
        root_metadata: &Metadata,
        path: &Path,
        file_metadata: &Metadata,
    ) -> io::Result<Self> {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let events = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fcntl(events.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut guard = Self {
            events,
            _files: Vec::new(),
            path: path.to_owned(),
        };
        let search = libc::O_SEARCH;
        let components: Vec<_> = path.components().collect();
        if components.first() != Some(&Component::RootDir) || !path.starts_with(root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid SQLite source path",
            ));
        }
        let mut selected = PathBuf::from("/");
        let fd = unsafe { libc::open(c"/".as_ptr(), search | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        guard._files.push(unsafe { File::from_raw_fd(fd) });
        for (index, component) in components.iter().enumerate().skip(1) {
            let Component::Normal(name) = component else {
                return Err(changed());
            };
            let directory = index + 1 < components.len();
            let name = CString::new(name.as_bytes()).map_err(|_| changed())?;
            let flags = libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if directory {
                    search | libc::O_DIRECTORY
                } else {
                    libc::O_RDONLY
                };
            let fd = unsafe {
                libc::openat(
                    guard._files.last().unwrap().as_raw_fd(),
                    name.as_ptr(),
                    flags,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            guard.watch(&file, directory)?;
            selected.push(component.as_os_str());
            let actual = file.metadata()?;
            let expected = if selected == root {
                Some(root_metadata)
            } else if !directory {
                Some(file_metadata)
            } else {
                None
            };
            if expected.is_some_and(|expected| {
                expected.dev() != actual.dev() || expected.ino() != actual.ino()
            }) {
                return Err(changed());
            }
            guard._files.push(file);
        }
        guard.check()?;
        Ok(guard)
    }

    fn watch(&self, file: &File, directory: bool) -> io::Result<()> {
        {
            let event = libc::kevent {
                ident: file.as_raw_fd() as _,
                filter: libc::EVFILT_VNODE,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_RENAME
                    | libc::NOTE_DELETE
                    | libc::NOTE_REVOKE
                    | if directory { 0 } else { libc::NOTE_LINK },
                data: 0,
                udata: std::ptr::null_mut(),
            };
            if unsafe {
                libc::kevent(
                    self.events.as_raw_fd(),
                    &event,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub(super) fn check(&self) -> io::Result<()> {
        // Check persistent replacement even if a namespace notification is still
        // being delivered. Compare the whole chain, including sidecar parents.
        let mut parent = self._files[0].try_clone()?;
        for (index, component) in self.path.components().enumerate().skip(1) {
            let Component::Normal(name) = component else {
                return Err(changed());
            };
            let name = CString::new(name.as_bytes()).map_err(|_| changed())?;
            let directory = index + 1 < self._files.len();
            let flags = libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if directory {
                    libc::O_SEARCH | libc::O_DIRECTORY
                } else {
                    libc::O_RDONLY
                };
            let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            parent = unsafe { File::from_raw_fd(fd) };
            let actual = parent.metadata()?;
            let expected = self._files[index].metadata()?;
            if actual.dev() != expected.dev() || actual.ino() != expected.ino() {
                return Err(changed());
            }
        }
        loop {
            let count = {
                let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
                let timeout = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                (unsafe {
                    libc::kevent(
                        self.events.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        event.as_mut_ptr(),
                        1,
                        &timeout,
                    )
                }) as isize
            };
            if count > 0 {
                return Err(changed());
            }
            if count == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_sqlite_guard_detects_ancestor_aba_but_allows_database_writes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("home");
        std::fs::create_dir_all(root.join("state")).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("state/history.db");
        std::fs::write(&path, b"original").unwrap();
        let guard = SqlitePathGuard::new(
            &root,
            &root.metadata().unwrap(),
            &path,
            &path.metadata().unwrap(),
        )
        .unwrap();
        std::fs::write(&path, b"updated").unwrap();
        guard.check().unwrap();
        let saved = fixture.path().join("saved");
        std::fs::rename(&root, &saved).unwrap();
        std::fs::rename(&saved, &root).unwrap();
        assert!(guard.check().is_err());
    }
}
