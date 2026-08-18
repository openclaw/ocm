#[cfg(target_os = "macos")]
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[cfg(not(target_os = "macos"))]
use filetime::{FileTime, set_file_times, set_symlink_file_times};
use rusqlite::{Connection, ErrorCode, OpenFlags};
#[cfg(not(target_os = "macos"))]
use std::io::Write;

use super::common::{ensure_dir, path_exists};
use super::layout::display_path;
use crate::infra::tree_digest::inventory_tree;

pub(crate) const STORAGE_APFS_CLONE: &str = "apfs-clone-v1";
pub(crate) const STORAGE_FULL_COPY: &str = "full-copy-v1";
pub(crate) const STORAGE_TAR_ARCHIVE: &str = "tar-archive-v1";

#[derive(Debug)]
pub(crate) struct PreparedTreeCheckpoint {
    source: PathBuf,
    #[cfg(target_os = "macos")]
    regular_files: HashMap<Box<[u8]>, PreparedRegularFile>,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileFingerprint {
    device: u64,
    inode: u64,
    len: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    mode: u32,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
struct PreparedRegularFile {
    fingerprint: Option<FileFingerprint>,
    sqlite: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SqliteCheck {
    NotDatabase,
    Verified,
    Deferred,
}

pub(crate) fn default_snapshot_storage_kind() -> String {
    STORAGE_TAR_ARCHIVE.to_string()
}

pub(crate) fn create_tree_checkpoint(source: &Path, destination: &Path) -> Result<String, String> {
    let prepared = prepare_tree_checkpoint(source)?;
    create_tree_checkpoint_from_preparation(prepared, destination)
}

pub(crate) fn prepare_tree_checkpoint(source: &Path) -> Result<PreparedTreeCheckpoint, String> {
    if !path_exists(source) {
        return Err(format!(
            "checkpoint source does not exist: {}",
            display_path(source)
        ));
    }

    #[cfg(target_os = "macos")]
    let regular_files = prepare_regular_files(source)?;
    #[cfg(not(target_os = "macos"))]
    preflight_sqlite_databases(source)?;

    Ok(PreparedTreeCheckpoint {
        source: source.to_path_buf(),
        #[cfg(target_os = "macos")]
        regular_files,
    })
}

pub(crate) fn create_tree_checkpoint_from_preparation(
    prepared: PreparedTreeCheckpoint,
    destination: &Path,
) -> Result<String, String> {
    if path_exists(destination) {
        return Err(format!(
            "checkpoint destination already exists: {}",
            display_path(destination)
        ));
    }
    if let Some(parent) = destination.parent() {
        ensure_dir(parent)?;
    }

    #[cfg(target_os = "macos")]
    {
        match clone_tree_checkpoint(&prepared.source, destination) {
            Ok(()) => {
                if let Err(error) = (|| {
                    verify_prepared_clone(&prepared.source, destination, prepared.regular_files)?;
                    sync_tree_root(destination)
                })() {
                    let _ = remove_tree_if_present(destination);
                    return Err(error);
                }
                return Ok(STORAGE_APFS_CLONE.to_string());
            }
            Err(clone_error) => {
                remove_tree_if_present(destination)?;
                copyfile_tree(&prepared.source, destination).map_err(|copy_error| {
                    format!(
                        "APFS clone was unavailable ({clone_error}); full checkpoint copy also failed: {copy_error}"
                    )
                })?;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    copy_tree_preserving_metadata(&prepared.source, destination)?;

    if let Err(error) = (|| {
        verify_tree_checkpoint(&prepared.source, destination)?;
        verify_sqlite_databases(destination)?;
        sync_tree_root(destination)
    })() {
        let _ = remove_tree_if_present(destination);
        return Err(error);
    }
    Ok(STORAGE_FULL_COPY.to_string())
}

pub(crate) fn copy_tree_checkpoint(source: &Path, destination: &Path) -> Result<(), String> {
    let _ = create_tree_checkpoint(source, destination)?;
    Ok(())
}

pub(crate) fn verify_tree_checkpoint(source: &Path, destination: &Path) -> Result<(), String> {
    let source_entries = inventory_tree(source)?;
    let destination_entries = inventory_tree(destination)?;
    if source_entries != destination_entries {
        return Err(format!(
            "checkpoint verification failed: {} does not exactly match {}",
            display_path(destination),
            display_path(source)
        ));
    }
    Ok(())
}

fn verify_sqlite_databases(root: &Path) -> Result<(), String> {
    for path in regular_files(root)? {
        let _ = verify_sqlite_database(&path)?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn preflight_sqlite_databases(root: &Path) -> Result<(), String> {
    for path in regular_files(root)? {
        let _ = check_sqlite_database(&path)?;
    }
    Ok(())
}

fn verify_sqlite_database(path: &Path) -> Result<bool, String> {
    match check_sqlite_database(path)? {
        SqliteCheck::NotDatabase => Ok(false),
        SqliteCheck::Verified => Ok(true),
        SqliteCheck::Deferred => Err(format!(
            "checkpoint SQLite database remained busy after quiescence: {}",
            display_path(path)
        )),
    }
}

fn check_sqlite_database(path: &Path) -> Result<SqliteCheck, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut magic = [0_u8; 16];
    if file.read_exact(&mut magic).is_err() || &magic != b"SQLite format 3\0" {
        return Ok(SqliteCheck::NotDatabase);
    }
    drop(file);
    let connection = match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(connection) => connection,
        Err(error) if sqlite_is_busy(&error) => return Ok(SqliteCheck::Deferred),
        Err(error) => {
            return Err(format!(
                "failed to open checkpoint SQLite database {}: {error}",
                display_path(path)
            ));
        }
    };
    let result: String = match connection.query_row("PRAGMA quick_check", [], |row| row.get(0)) {
        Ok(result) => result,
        Err(error) if sqlite_is_busy(&error) => return Ok(SqliteCheck::Deferred),
        Err(error) => {
            return Err(format!(
                "failed to verify checkpoint SQLite database {}: {error}",
                display_path(path)
            ));
        }
    };
    if result != "ok" {
        return Err(format!(
            "checkpoint SQLite integrity check failed for {}: {result}",
            display_path(path)
        ));
    }
    Ok(SqliteCheck::Verified)
}

fn sqlite_is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

#[cfg(target_os = "macos")]
fn prepare_regular_files(root: &Path) -> Result<HashMap<Box<[u8]>, PreparedRegularFile>, String> {
    let mut out = HashMap::new();
    prepare_regular_path(root, root, &mut out)?;
    Ok(out)
}

#[cfg(target_os = "macos")]
fn prepare_regular_path(
    root: &Path,
    path: &Path,
    out: &mut HashMap<Box<[u8]>, PreparedRegularFile>,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to inspect checkpoint source {}: {error}",
                display_path(path)
            ));
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Ok(());
    }
    if file_type.is_file() {
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .as_os_str()
            .as_bytes()
            .to_vec()
            .into_boxed_slice();
        let before = file_fingerprint(&metadata);
        let sqlite_check = match check_sqlite_database(path) {
            Ok(check) => check,
            Err(_) if !path_exists(path) => {
                out.insert(
                    relative,
                    PreparedRegularFile {
                        fingerprint: None,
                        sqlite: false,
                    },
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let after = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => Some(file_fingerprint(&metadata)),
            Ok(_) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        out.insert(
            relative,
            PreparedRegularFile {
                fingerprint: after.filter(|fingerprint| {
                    *fingerprint == before && sqlite_check != SqliteCheck::Deferred
                }),
                sqlite: sqlite_check != SqliteCheck::NotDatabase,
            },
        );
        return Ok(());
    }
    if file_type.is_dir() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries {
            match entry {
                Ok(entry) => prepare_regular_path(root, &entry.path(), out)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn file_fingerprint(metadata: &fs::Metadata) -> FileFingerprint {
    use std::os::unix::fs::MetadataExt;

    FileFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        len: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        mode: metadata.mode(),
    }
}

#[cfg(target_os = "macos")]
fn verify_prepared_clone(
    source: &Path,
    destination: &Path,
    mut prepared: HashMap<Box<[u8]>, PreparedRegularFile>,
) -> Result<(), String> {
    let mut candidates = HashSet::new();
    verify_prepared_path(source, source, destination, &mut prepared, &mut candidates)?;
    for (relative, prior) in prepared {
        let relative = PathBuf::from(std::ffi::OsStr::from_bytes(&relative).to_os_string());
        verify_changed_checkpoint_path(&source.join(&relative), &destination.join(&relative))?;
        if prior.sqlite && path_exists(&destination.join(&relative)) {
            candidates.insert(destination.join(&relative));
        }
        if let Some(primary) = sqlite_primary_for_sidecar(&relative) {
            candidates.insert(destination.join(primary));
        }
    }

    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort();
    for path in candidates {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if metadata.is_file() {
            let _ = verify_sqlite_database(&path)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_prepared_path(
    root: &Path,
    path: &Path,
    destination: &Path,
    prepared: &mut HashMap<Box<[u8]>, PreparedRegularFile>,
    sqlite_candidates: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
        let relative_bytes = relative.as_os_str().as_bytes();
        let current = file_fingerprint(&metadata);
        let prior = prepared.remove(relative_bytes);
        let unchanged = prior
            .and_then(|entry| entry.fingerprint)
            .is_some_and(|fingerprint| fingerprint == current);
        let destination_path = destination.join(relative);
        preserve_special_mode(&destination_path, &metadata)?;
        if !unchanged {
            verify_changed_checkpoint_path(path, &destination_path)?;
            if prior.is_some_and(|entry| entry.sqlite) || has_sqlite_magic(path)? {
                sqlite_candidates.insert(destination_path);
            }
            if let Some(primary) = sqlite_primary_for_sidecar(relative) {
                sqlite_candidates.insert(destination.join(primary));
            }
        }
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            verify_prepared_path(
                root,
                &entry.map_err(|error| error.to_string())?.path(),
                destination,
                prepared,
                sqlite_candidates,
            )?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn preserve_special_mode(destination: &Path, source_metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = source_metadata.mode();
    if mode & 0o6000 == 0 {
        return Ok(());
    }
    let mut permissions = fs::symlink_metadata(destination)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(destination, permissions)
        .and_then(|()| fs::File::open(destination)?.sync_all())
        .map_err(|error| {
            format!(
                "failed to preserve checkpoint mode for {}: {error}",
                display_path(destination)
            )
        })
}

#[cfg(target_os = "macos")]
fn preserve_special_modes(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        return preserve_special_mode(destination, &metadata);
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            preserve_special_modes(&entry.path(), &destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_changed_checkpoint_path(source: &Path, destination: &Path) -> Result<(), String> {
    let source_exists = path_exists(source);
    let destination_exists = path_exists(destination);
    if !source_exists && !destination_exists {
        return Ok(());
    }
    if source_exists
        && destination_exists
        && inventory_tree(source)? == inventory_tree(destination)?
    {
        return Ok(());
    }
    Err(format!(
        "checkpoint verification failed: {} does not exactly match changed source {}",
        display_path(destination),
        display_path(source)
    ))
}

#[cfg(target_os = "macos")]
fn has_sqlite_magic(path: &Path) -> Result<bool, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut magic = [0_u8; 16];
    Ok(file.read_exact(&mut magic).is_ok() && &magic == b"SQLite format 3\0")
}

#[cfg(target_os = "macos")]
fn sqlite_primary_for_sidecar(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?.to_str()?;
    for suffix in ["-wal", "-shm", "-journal"] {
        if let Some(primary_name) = file_name.strip_suffix(suffix)
            && !primary_name.is_empty()
        {
            return Some(path.with_file_name(primary_name));
        }
    }
    None
}

fn regular_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    collect_regular_files(root, &mut out)?;
    Ok(out)
}

fn collect_regular_files(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            collect_regular_files(&entry.map_err(|error| error.to_string())?.path(), out)?;
        }
    }
    Ok(())
}

fn sync_tree_root(root: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = root;
        return Ok(());
    }
    #[cfg(not(windows))]
    fs::File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("failed to sync checkpoint {}: {error}", display_path(root)))
}

pub(crate) fn remove_tree_if_present(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            make_tree_removable(path)?;
            fs::remove_dir_all(path).map_err(|error| error.to_string())
        }
        Ok(_) => fs::remove_file(path).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn make_tree_removable(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.is_dir() {
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o700);
            fs::set_permissions(path, permissions).map_err(|error| error.to_string())?;
        }
    }
    #[cfg(windows)]
    {
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).map_err(|error| error.to_string())?;
        }
    }

    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            make_tree_removable(&entry.map_err(|error| error.to_string())?.path())?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
type CopyfileState = *mut libc::c_void;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn copyfile(
        from: *const libc::c_char,
        to: *const libc::c_char,
        state: CopyfileState,
        flags: u32,
    ) -> libc::c_int;
    fn copyfile_state_alloc() -> CopyfileState;
    fn copyfile_state_free(state: CopyfileState) -> libc::c_int;
    fn copyfile_state_set(
        state: CopyfileState,
        flag: u32,
        value: *const libc::c_void,
    ) -> libc::c_int;
    fn clonefile(
        source: *const libc::c_char,
        destination: *const libc::c_char,
        flags: u32,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn clone_tree_checkpoint(source: &Path, destination: &Path) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    const CLONE_ACL: u32 = 1 << 2;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| "checkpoint destination has no parent".to_string())?;
    if fs::metadata(source)
        .map_err(|error| error.to_string())?
        .dev()
        != fs::metadata(destination_parent)
            .map_err(|error| error.to_string())?
            .dev()
    {
        return Err("source and checkpoint destination are on different filesystems".to_string());
    }
    let source_c =
        CString::new(source.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let destination_c =
        CString::new(destination.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    // Directory clonefile is all-or-nothing and never falls back to byte copying.
    let result = unsafe { clonefile(source_c.as_ptr(), destination_c.as_ptr(), CLONE_ACL) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copyfile_tree(source: &Path, destination: &Path) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const COPYFILE_ALL: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
    const COPYFILE_RECURSIVE: u32 = 1 << 15;
    const COPYFILE_NOFOLLOW_SRC: u32 = 1 << 18;
    const COPYFILE_STATE_PRESERVE_SUID: u32 = 16;

    let source_c =
        CString::new(source.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let destination_c =
        CString::new(destination.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let state = unsafe { copyfile_state_alloc() };
    if state.is_null() {
        return Err("failed to allocate copyfile state".to_string());
    }
    let preserve_suid = 1_u32;
    let state_result = unsafe {
        copyfile_state_set(
            state,
            COPYFILE_STATE_PRESERVE_SUID,
            std::ptr::from_ref(&preserve_suid).cast(),
        )
    };
    if state_result != 0 {
        unsafe {
            copyfile_state_free(state);
        }
        return Err("failed to configure copyfile mode preservation".to_string());
    }
    let result = unsafe {
        copyfile(
            source_c.as_ptr(),
            destination_c.as_ptr(),
            state,
            COPYFILE_ALL | COPYFILE_RECURSIVE | COPYFILE_NOFOLLOW_SRC,
        )
    };
    unsafe {
        copyfile_state_free(state);
    }
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    preserve_special_modes(source, destination)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn copy_tree_preserving_metadata(source: &Path, destination: &Path) -> Result<(), String> {
    copy_path_preserving_metadata(source, destination)
}

#[cfg(not(target_os = "macos"))]
fn copy_path_preserving_metadata(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(source).map_err(|error| error.to_string())?;
        if let Some(parent) = destination.parent() {
            ensure_dir(parent)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, destination).map_err(|error| error.to_string())?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, destination)
            .or_else(|_| std::os::windows::fs::symlink_dir(target, destination))
            .map_err(|error| error.to_string())?;
        preserve_metadata(source, destination, &metadata, true)?;
        return Ok(());
    }
    if file_type.is_dir() {
        ensure_dir(destination)?;
        let entries = fs::read_dir(source)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        for entry in entries {
            copy_path_preserving_metadata(&entry.path(), &destination.join(entry.file_name()))?;
        }
        preserve_metadata(source, destination, &metadata, false)?;
        return Ok(());
    }
    if file_type.is_file() {
        if let Some(parent) = destination.parent() {
            ensure_dir(parent)?;
        }
        let mut input = fs::File::open(source).map_err(|error| error.to_string())?;
        let mut output = fs::File::create(destination).map_err(|error| error.to_string())?;
        std::io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
        output.flush().map_err(|error| error.to_string())?;
        output.sync_all().map_err(|error| error.to_string())?;
        preserve_metadata(source, destination, &metadata, false)?;
        return Ok(());
    }
    Err(format!(
        "unsupported special file in checkpoint: {}",
        display_path(source)
    ))
}

#[cfg(not(target_os = "macos"))]
fn preserve_metadata(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    symlink: bool,
) -> Result<(), String> {
    if !symlink {
        fs::set_permissions(destination, metadata.permissions())
            .map_err(|error| error.to_string())?;
    }
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);
    if symlink {
        set_symlink_file_times(destination, accessed, modified)
            .map_err(|error| error.to_string())?;
    } else {
        set_file_times(destination, accessed, modified).map_err(|error| error.to_string())?;
    }
    #[cfg(unix)]
    for name in xattr::list(source).map_err(|error| error.to_string())? {
        if let Some(value) = xattr::get(source, &name).map_err(|error| error.to_string())? {
            xattr::set(destination, &name, &value).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    #[cfg(target_os = "macos")]
    use super::{
        STORAGE_APFS_CLONE, copyfile_tree, create_tree_checkpoint_from_preparation,
        prepare_tree_checkpoint, sqlite_primary_for_sidecar, verify_tree_checkpoint,
    };
    use crate::infra::tree_digest::inventory_tree;

    #[test]
    fn checkpoint_inventory_ignores_directory_allocation_lengths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/value.txt"), "value\n").unwrap();

        let inventory = inventory_tree(root.path()).unwrap();
        assert_eq!(inventory[Path::new("")].len, None);
        assert_eq!(inventory[Path::new("nested")].len, None);
        assert_eq!(inventory[Path::new("nested/value.txt")].len, Some(6));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apfs_checkpoint_atomically_clones_the_complete_hierarchy() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let source = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("generated/node_modules/pkg")).unwrap();
        fs::write(source.path().join("empty"), []).unwrap();
        fs::write(
            source.path().join("generated/node_modules/pkg/index.js"),
            "module.exports = 1;\n",
        )
        .unwrap();
        fs::write(source.path().join("privileged-tool"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(
            source.path().join("privileged-tool"),
            fs::Permissions::from_mode(0o4755),
        )
        .unwrap();
        let destination_parent = tempfile::tempdir().unwrap();
        let destination = destination_parent.path().join("checkpoint");

        let prepared = prepare_tree_checkpoint(source.path()).unwrap();
        let storage = create_tree_checkpoint_from_preparation(prepared, &destination).unwrap();

        assert_eq!(storage, STORAGE_APFS_CLONE);
        verify_tree_checkpoint(source.path(), &destination).unwrap();
        assert_eq!(
            fs::metadata(destination.join("privileged-tool"))
                .unwrap()
                .mode()
                & 0o7777,
            0o4755
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prepared_checkpoint_rechecks_same_size_sqlite_mutation() {
        let source = tempfile::tempdir().unwrap();
        let database_path = source.path().join("state.sqlite");
        let database = rusqlite::Connection::open(&database_path).unwrap();
        database
            .execute_batch(
                "CREATE TABLE state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO state VALUES ('sentinel', 'before');",
            )
            .unwrap();
        drop(database);
        let prepared = prepare_tree_checkpoint(source.path()).unwrap();
        let metadata = fs::metadata(&database_path).unwrap();
        let original_len = metadata.len() as usize;
        let modified = filetime::FileTime::from_last_modification_time(&metadata);
        let mut corrupt = vec![0_u8; original_len];
        corrupt[..16].copy_from_slice(b"SQLite format 3\0");
        fs::write(&database_path, corrupt).unwrap();
        filetime::set_file_mtime(&database_path, modified).unwrap();

        let destination_parent = tempfile::tempdir().unwrap();
        let destination = destination_parent.path().join("checkpoint");
        let error = create_tree_checkpoint_from_preparation(prepared, &destination).unwrap_err();

        assert!(error.contains("SQLite"), "{error}");
        assert!(!destination.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prepared_checkpoint_defers_busy_sqlite_until_capture() {
        let source = tempfile::tempdir().unwrap();
        let database_path = source.path().join("state.sqlite");
        let database = rusqlite::Connection::open(&database_path).unwrap();
        database
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 CREATE TABLE state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO state VALUES ('sentinel', 'before');
                 BEGIN EXCLUSIVE;",
            )
            .unwrap();

        let prepared = prepare_tree_checkpoint(source.path()).unwrap();
        database.execute_batch("ROLLBACK").unwrap();
        drop(database);
        let destination_parent = tempfile::tempdir().unwrap();
        let destination = destination_parent.path().join("checkpoint");

        let storage = create_tree_checkpoint_from_preparation(prepared, &destination).unwrap();

        assert_eq!(storage, STORAGE_APFS_CLONE);
        verify_tree_checkpoint(source.path(), &destination).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sqlite_sidecars_revalidate_the_primary_database() {
        assert_eq!(
            sqlite_primary_for_sidecar(Path::new("state/openclaw.sqlite-wal")),
            Some(Path::new("state/openclaw.sqlite").to_path_buf())
        );
        assert_eq!(
            sqlite_primary_for_sidecar(Path::new("state/openclaw.sqlite-shm")),
            Some(Path::new("state/openclaw.sqlite").to_path_buf())
        );
        assert_eq!(
            sqlite_primary_for_sidecar(Path::new("state/openclaw.sqlite-journal")),
            Some(Path::new("state/openclaw.sqlite").to_path_buf())
        );
        assert_eq!(
            sqlite_primary_for_sidecar(Path::new("state/openclaw.sqlite")),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_full_copy_preserves_symlinks() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("state.txt"), "before\n").unwrap();
        fs::write(source.path().join("privileged-tool"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(
            source.path().join("privileged-tool"),
            fs::Permissions::from_mode(0o4755),
        )
        .unwrap();
        symlink("state.txt", source.path().join("current")).unwrap();
        let destination_parent = tempfile::tempdir().unwrap();
        let destination = destination_parent.path().join("checkpoint");

        copyfile_tree(source.path(), &destination).unwrap();

        assert!(
            fs::symlink_metadata(destination.join("current"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(destination.join("current")).unwrap(),
            Path::new("state.txt")
        );
        assert_eq!(
            fs::metadata(destination.join("privileged-tool"))
                .unwrap()
                .mode()
                & 0o7777,
            0o4755
        );
        verify_tree_checkpoint(source.path(), &destination).unwrap();
    }
}
