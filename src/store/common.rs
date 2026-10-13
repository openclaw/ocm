use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::openclaw_state::{OpenClawSqliteCopy, is_sqlite_database};
use crate::infra::sqlite_snapshot::create_sqlite_snapshot;

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

pub(crate) fn path_exists(path: &Path) -> bool {
    path.exists()
}

pub(crate) fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())
}

#[derive(Debug)]
pub(crate) struct ExclusiveFileLock {
    file: File,
    child_custody: AtomicBool,
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        // An unrelated fork can retain CLOEXEC descriptors until it execs.
        // Only deliberate child custody may extend this guard's lock lifetime.
        if !self.child_custody.load(Ordering::Relaxed) {
            let _ = FileExt::unlock(&self.file);
        }
    }
}

impl ExclusiveFileLock {
    pub(crate) fn output(
        &self,
        mut command: std::process::Command,
    ) -> std::io::Result<std::process::Output> {
        self.retain_for_child(&mut command);
        command.output()
    }

    pub(crate) fn retain_for_child(&self, command: &mut std::process::Command) {
        retain_lock_for_child(&self.file, &self.child_custody, command);
    }
}

#[derive(Debug)]
pub(crate) struct SharedFileLock {
    file: File,
    child_custody: AtomicBool,
}

impl Drop for SharedFileLock {
    fn drop(&mut self) {
        if !self.child_custody.load(Ordering::Relaxed) {
            let _ = FileExt::unlock(&self.file);
        }
    }
}

impl SharedFileLock {
    pub(crate) fn retain_for_child(&self, command: &mut std::process::Command) {
        retain_lock_for_child(&self.file, &self.child_custody, command);
    }
}

fn retain_lock_for_child(
    file: &File,
    child_custody: &AtomicBool,
    command: &mut std::process::Command,
) {
    #[cfg(not(unix))]
    let _ = (file, child_custody, command);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        // Keep this set on output errors too: a started child or descendant
        // may still hold the lock, which must then release on last close.
        child_custody.store(true, Ordering::Relaxed);
        let fd = file.as_raw_fd();
        // Only this child's copy becomes inheritable, so concurrent fleet
        // workers and service spawns cannot retain another operation's lock across exec.
        // SAFETY: the caller keeps file open through command execution; fcntl
        // is async-signal-safe and does not allocate after fork.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

pub(crate) fn lock_file(path: &Path, label: &str) -> Result<ExclusiveFileLock, String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open {label} lock at {}: {error}", path.display()))?;
    file.lock_exclusive().map_err(|error| {
        format!(
            "failed to acquire {label} lock at {}: {error}",
            path.display()
        )
    })?;
    Ok(ExclusiveFileLock {
        file,
        child_custody: AtomicBool::new(false),
    })
}

pub(crate) fn try_lock_file(path: &Path, label: &str) -> Result<Option<ExclusiveFileLock>, String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open {label} lock at {}: {error}", path.display()))?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(ExclusiveFileLock {
            file,
            child_custody: AtomicBool::new(false),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(format!(
            "failed to acquire {label} lock at {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn lock_file_shared(path: &Path, label: &str) -> Result<SharedFileLock, String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open {label} lock at {}: {error}", path.display()))?;
    FileExt::lock_shared(&file).map_err(|error| {
        format!(
            "failed to acquire shared {label} lock at {}: {error}",
            path.display()
        )
    })?;
    Ok(SharedFileLock {
        file,
        child_custody: AtomicBool::new(false),
    })
}

pub(crate) fn try_lock_file_shared(
    path: &Path,
    label: &str,
) -> Result<Option<SharedFileLock>, String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open {label} lock at {}: {error}", path.display()))?;
    match FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(Some(SharedFileLock {
            file,
            child_custody: AtomicBool::new(false),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(format!(
            "failed to acquire shared {label} lock at {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    copy_dir_recursive_inner(source, destination, None)
}

// Clone and migration opt in; package, archive restore, and checkpoint callers
// retain the raw copy contract. Protected historical payloads remain byte copies.
pub(crate) fn copy_dir_with_sqlite_snapshots(
    source: &Path,
    destination: &Path,
    state_root: &Path,
) -> Result<(), String> {
    OpenClawDirectoryCopy::new(state_root)?.copy(source, destination)
}

// One operation retains destination ownership through every migration overlay.
// Incoming workspace names cannot turn a completed snapshot back into a raw copy.
pub(crate) struct OpenClawDirectoryCopy<'a> {
    selection: OpenClawSqliteCopy<'a>,
    databases: HashSet<PathBuf>,
}

impl<'a> OpenClawDirectoryCopy<'a> {
    pub(crate) fn new(state_root: &'a Path) -> Result<Self, String> {
        Ok(Self {
            selection: OpenClawSqliteCopy::new(state_root)?,
            databases: HashSet::new(),
        })
    }

    pub(crate) fn copy(&mut self, source: &Path, destination: &Path) -> Result<(), String> {
        copy_dir_recursive_inner(source, destination, Some(self))
    }
}

fn copy_dir_recursive_inner(
    source: &Path,
    destination: &Path,
    mut copying: Option<&mut OpenClawDirectoryCopy<'_>>,
) -> Result<(), String> {
    ensure_dir(destination)?;

    let entries = fs::read_dir(source)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            Ok((entry.path(), destination.join(entry.file_name())))
        })
        .collect::<Result<Vec<_>, String>>()?;

    // Enumerate before copying: a sidecar may precede its primary, or disappear
    // during a checkpoint. Never attach raw sidecars to a completed snapshot.
    let mut databases = HashSet::new();
    let mut sidecars = HashSet::new();
    if let Some(copying) = copying.as_ref() {
        for (path, target) in &entries {
            if (copying.selection.contains(path) || copying.databases.contains(target))
                && fs::symlink_metadata(path)
                    .map_err(|error| error.to_string())?
                    .is_file()
                && is_sqlite_database(path)?
            {
                databases.insert(path.clone());
                sidecars.extend(sqlite_sidecars(path));
            }
        }
    }
    let mut existing_sidecars = HashSet::new();
    if let Some(copying) = copying.as_ref() {
        for entry in fs::read_dir(destination).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if (copying.selection.contains(&source.join(entry.file_name()))
                || copying.databases.contains(&entry.path()))
                && entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_file()
                && is_sqlite_database(&entry.path())?
            {
                existing_sidecars.extend(sqlite_sidecars(&entry.path()));
            }
        }
    }
    for (source_path, destination_path) in entries {
        if sidecars.contains(&source_path) {
            continue;
        }
        if databases.contains(&source_path) {
            let permissions = fs::metadata(&source_path)
                .map_err(|error| error.to_string())?
                .permissions();
            let snapshot = create_sqlite_snapshot(&source_path)?;
            // An external-workspace overlay can replace an earlier home copy.
            // Its old WAL/journal must not be replayed over the new snapshot.
            for sidecar in sqlite_sidecars(&destination_path) {
                match fs::remove_file(&sidecar) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            fs::copy(snapshot.path(), &destination_path).map_err(|error| error.to_string())?;
            fs::set_permissions(&destination_path, permissions)
                .map_err(|error| error.to_string())?;
            if let Some(copying) = copying.as_deref_mut() {
                copying.databases.insert(destination_path);
            }
        } else {
            if existing_sidecars.contains(&destination_path) {
                return Err(format!(
                    "cannot overlay SQLite sidecar without its source database: {}",
                    source_path.display()
                ));
            }
            if copying
                .as_ref()
                .is_some_and(|copying| copying.databases.contains(&destination_path))
            {
                return Err(format!(
                    "cannot replace copied SQLite database with a non-database: {}",
                    source_path.display()
                ));
            }
            copy_path_inner(&source_path, &destination_path, copying.as_deref_mut())?;
        }
    }

    Ok(())
}

fn sqlite_sidecars(path: &Path) -> [PathBuf; 3] {
    ["-wal", "-shm", "-journal"].map(|suffix| {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    })
}

pub(crate) fn copy_path_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    copy_path(source, destination)
}

pub(crate) fn copy_path(source: &Path, destination: &Path) -> Result<(), String> {
    copy_path_inner(source, destination, None)
}

fn copy_path_inner(
    source: &Path,
    destination: &Path,
    copying: Option<&mut OpenClawDirectoryCopy<'_>>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        return copy_symlink(source, destination);
    }
    if file_type.is_dir() {
        return copy_dir_recursive_inner(source, destination, copying);
    }
    #[cfg(unix)]
    if file_type.is_socket() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        ensure_dir(parent)?;
    }
    fs::copy(source, destination)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn copy_symlink(source_path: &Path, destination_path: &Path) -> Result<(), String> {
    if let Some(parent) = destination_path.parent() {
        ensure_dir(parent)?;
    }
    let target = fs::read_link(source_path).map_err(|error| error.to_string())?;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, destination_path).map_err(|error| error.to_string())?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(&target, destination_path)
            .or_else(|_| std::os::windows::fs::symlink_dir(&target, destination_path))
            .map_err(|error| error.to_string())?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        return Err(format!(
            "copying symlinks is not supported on this platform: {}",
            source_path.display()
        ));
    }

    Ok(())
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let mut raw = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    raw.push('\n');
    write_file_replacing_path(path, raw.as_bytes())
}

pub(crate) fn write_file_replacing_path(path: &Path, raw: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("path has no file name: {}", path.display()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let temp_path = parent.join(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()));

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        file.write_all(raw).map_err(|error| error.to_string())?;
        drop(file);
        replace_path(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(not(windows))]
pub(super) fn replace_path(source: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(source, destination).map_err(|error| error.to_string())
}

#[cfg(windows)]
pub(super) fn replace_path(source: &Path, destination: &Path) -> Result<(), String> {
    let mut source_wide = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source_wide.push(0);
    let mut destination_wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    destination_wide.push(0);
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    let moved = unsafe { MoveFileExW(source_wide.as_ptr(), destination_wide.as_ptr(), flags) };
    if moved == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

pub(crate) fn load_json_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error.to_string()),
    };

    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value == "json")
                .unwrap_or(false)
        {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::{copy_dir_recursive, copy_dir_with_sqlite_snapshots, write_file_replacing_path};

    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join("ocm-copy-dir-tests")
            .join(format!("{label}-{}-{id}", std::process::id()))
    }

    #[cfg(unix)]
    #[test]
    fn inherited_lock_survives_parent_guard_drop() {
        assert_inherited_lock_survives_parent_guard_drop(false);
        assert_inherited_lock_survives_parent_guard_drop(true);
    }

    #[cfg(unix)]
    fn assert_inherited_lock_survives_parent_guard_drop(shared: bool) {
        use std::process::Command;
        use std::thread::sleep;
        use std::time::{Duration, Instant};

        let root = temp_root("inherited-lock");
        fs::create_dir_all(&root).unwrap();
        let lock_path = root.join("lock");
        let started = root.join("started");
        let release = root.join("release");
        struct ReleaseChild(PathBuf);
        impl Drop for ReleaseChild {
            fn drop(&mut self) {
                let _ = fs::write(&self.0, "");
            }
        }
        let _release_child = ReleaseChild(release.clone());
        let exclusive_lock = (!shared).then(|| super::lock_file(&lock_path, "test").unwrap());
        let shared_lock = shared.then(|| super::lock_file_shared(&lock_path, "test").unwrap());
        let mut command = Command::new("sh");
        command.args(["-c", "(touch \"$1\"; count=0; while [ ! -e \"$2\" ] && [ $count -lt 100 ]; do sleep 0.05; count=$((count + 1)); done) </dev/null >/dev/null 2>&1 &", "sh"])
            .arg(&started).arg(&release);
        if let Some(lock) = &exclusive_lock {
            lock.retain_for_child(&mut command);
        }
        if let Some(lock) = &shared_lock {
            lock.retain_for_child(&mut command);
        }
        assert!(command.output().unwrap().status.success());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !started.exists() {
            assert!(Instant::now() < deadline, "inheriting child did not start");
            sleep(Duration::from_millis(10));
        }
        drop((exclusive_lock, shared_lock));
        if shared {
            // Dropping a compatible reader must not release the child's lock.
            assert!(
                super::try_lock_file_shared(&lock_path, "reader")
                    .unwrap()
                    .is_some()
            );
        }
        assert!(super::try_lock_file(&lock_path, "test").unwrap().is_none());
        fs::write(&release, "").unwrap();
        loop {
            if super::try_lock_file(&lock_path, "test").unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "child did not release its lock");
            sleep(Duration::from_millis(10));
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_recursive_preserves_broken_symlinks() {
        let root = temp_root("broken-symlink");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source.join("node_modules/.bin")).unwrap();
        fs::write(source.join("node_modules/package.json"), "{}\n").unwrap();
        std::os::unix::fs::symlink(
            "../missing-package/bin/tool",
            source.join("node_modules/.bin/tool"),
        )
        .unwrap();

        copy_dir_recursive(&source, &destination).unwrap();

        let copied_link = destination.join("node_modules/.bin/tool");
        let metadata = fs::symlink_metadata(&copied_link).unwrap();
        assert!(metadata.file_type().is_symlink());
        assert_eq!(
            fs::read_link(&copied_link).unwrap(),
            PathBuf::from("../missing-package/bin/tool")
        );
        assert!(!copied_link.exists());
        assert_eq!(
            fs::read_to_string(destination.join("node_modules/package.json")).unwrap(),
            "{}\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_recursive_copies_deep_trees_without_retaining_parent_handles() {
        let root = temp_root("deep-tree");
        let source = root.join("source");
        let destination = root.join("destination");
        let mut current = source.clone();
        for index in 0..128 {
            current = current.join(format!("d{index}"));
            fs::create_dir_all(&current).unwrap();
            fs::write(current.join("note.txt"), format!("level {index}\n")).unwrap();
        }

        copy_dir_recursive(&source, &destination).unwrap();

        let deepest = (0..128).fold(destination, |path, index| path.join(format!("d{index}")));
        assert_eq!(
            fs::read_to_string(deepest.join("note.txt")).unwrap(),
            "level 127\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_recursive_skips_unix_sockets() {
        use std::os::unix::net::UnixListener;

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from("/tmp").join(format!("ocm-socket-{}-{id}", std::process::id()));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("note.txt"), "copied\n").unwrap();
        let socket_path = source.join("service.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        copy_dir_recursive(&source, &destination).unwrap();

        assert_eq!(
            fs::read_to_string(destination.join("note.txt")).unwrap(),
            "copied\n"
        );
        assert!(!destination.join("service.sock").exists());

        drop(listener);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_file_replacing_path_replaces_a_symlink_without_touching_its_target() {
        let root = temp_root("replace-symlink");
        let external = root.join("external.json");
        let owned = root.join("owned.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(&external, "external\n").unwrap();
        std::os::unix::fs::symlink(&external, &owned).unwrap();

        write_file_replacing_path(&owned, b"owned\n").unwrap();

        assert_eq!(fs::read_to_string(&external).unwrap(), "external\n");
        assert_eq!(fs::read_to_string(&owned).unwrap(), "owned\n");
        assert!(fs::symlink_metadata(&owned).unwrap().file_type().is_file());

        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sqlite_copy_preserves_opaque_history_and_raw_copy_contract() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let raw = root.path().join("raw");
        let target = root.path().join("target");
        let invalid = b"SQLite format 3\0opaque historical payload";
        for relative in [
            "skill-workshop/proposals/example/support/data.sqlite",
            "audit/system-agent.jsonl.migrated.2",
        ] {
            let path = source.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, invalid).unwrap();
        }
        fs::write(source.join("opaque.sqlite"), b"not a database").unwrap();
        for relative in [
            "extensions/example/fixture.sqlite",
            "plugins/example/fixture.sqlite",
            "workspace/fixture.sqlite",
            "imports/archived.sqlite",
        ] {
            let path = source.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, invalid).unwrap();
        }
        let operational = [
            "state/openclaw-quarantine.sqlite",
            "agents/main/agent/openclaw-agent.sqlite",
            "plugins/workboard/workboard.sqlite",
            "plugins/team-reports/team-reports.sqlite",
            "logbook/logbook.sqlite",
            "matrix/accounts/default/server__user/0123456789abcdef/state/openclaw.sqlite",
        ];
        let mut writers = Vec::new();
        for relative in operational {
            let path = source.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE durable(value); INSERT INTO durable VALUES(42);").unwrap();
            writers.push(db);
        }

        copy_dir_with_sqlite_snapshots(&source, &target, &source).unwrap();
        for relative in [
            "skill-workshop/proposals/example/support/data.sqlite",
            "audit/system-agent.jsonl.migrated.2",
        ] {
            assert_eq!(fs::read(target.join(relative)).unwrap(), invalid);
        }
        assert_eq!(
            fs::read(target.join("opaque.sqlite")).unwrap(),
            b"not a database"
        );
        for relative in [
            "extensions/example/fixture.sqlite",
            "plugins/example/fixture.sqlite",
            "workspace/fixture.sqlite",
            "imports/archived.sqlite",
        ] {
            assert_eq!(fs::read(target.join(relative)).unwrap(), invalid);
        }
        for relative in operational {
            let path = target.join(relative);
            assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
            let db = rusqlite::Connection::open(path).unwrap();
            assert_eq!(
                db.query_row("SELECT value FROM durable", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                42
            );
        }
        fs::write(source.join("broken.sqlite"), invalid).unwrap();
        fs::write(source.join("broken.sqlite-wal"), b"raw sidecar").unwrap();
        copy_dir_recursive(&source, &raw).unwrap();
        assert_eq!(fs::read(raw.join("broken.sqlite")).unwrap(), invalid);
        assert_eq!(
            fs::read(raw.join("broken.sqlite-wal")).unwrap(),
            b"raw sidecar"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_copy_preserves_permissions_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(source.join("memory")).unwrap();
        let path = source.join("memory/main.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE durable(value); INSERT INTO durable VALUES(42);",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        symlink("missing", source.join("broken.sqlite")).unwrap();
        symlink("memory/main.sqlite", source.join("linked.sqlite")).unwrap();
        copy_dir_with_sqlite_snapshots(&source, &target, &source).unwrap();
        assert_eq!(
            fs::metadata(target.join("memory/main.sqlite"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            fs::read_link(target.join("broken.sqlite")).unwrap(),
            PathBuf::from("missing")
        );
        assert_eq!(
            fs::read_link(target.join("linked.sqlite")).unwrap(),
            PathBuf::from("memory/main.sqlite")
        );
        assert!(!target.join("memory/main.sqlite-wal").exists());
        let copied = rusqlite::Connection::open(target.join("memory/main.sqlite")).unwrap();
        assert_eq!(
            copied
                .query_row("SELECT value FROM durable", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
    }
}
