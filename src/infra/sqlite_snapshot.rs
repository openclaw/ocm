use std::ffi::CStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};

const SQLITE_BACKUP_PAGES_PER_STEP: i32 = 256;
const SQLITE_BACKUP_RETRY_DELAY: Duration = Duration::from_millis(10);
const SQLITE_BACKUP_TIMEOUT: Duration = Duration::from_secs(30);
const RECOVERED_DATABASE: &str = "database";

pub(crate) struct SqliteSnapshot {
    file: NamedTempFile,
}

impl SqliteSnapshot {
    pub(crate) fn path(&self) -> &Path {
        self.file.path()
    }
}

pub(crate) fn file_is_sqlite_database(file: &mut File) -> Result<bool, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut magic = [0_u8; 16];
    let database = file.read_exact(&mut magic).is_ok() && &magic == b"SQLite format 3\0";
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    Ok(database)
}

/// A read-only connection cannot replay a hot rollback journal, so SQLite
/// refuses to read the database until a writer recovers it.
pub(crate) fn sqlite_needs_rollback(error: &rusqlite::Error) -> bool {
    error
        .sqlite_error()
        .is_some_and(|error| error.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK)
}

pub(crate) fn sqlite_is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// The committed state of a database left with a hot rollback journal,
/// recovered by replaying the journal in a private copy.
pub(crate) struct RecoveredSqliteDatabase {
    directory: TempDir,
    source_digest: [u8; 32],
}

impl RecoveredSqliteDatabase {
    pub(crate) fn path(&self) -> PathBuf {
        self.directory.path().join(RECOVERED_DATABASE)
    }

    /// Whether `database` and its journal hold exactly the recovered source bytes.
    pub(crate) fn matches(&self, database: &Path) -> Result<bool, String> {
        Ok(database_digest(database, false)? == Some(self.source_digest))
    }
}

/// Recovers the committed state of `source` while its rollback journal is hot,
/// without opening the live database or journal for writing. Returns `None`
/// when the journal is not hot or changes during the copy: another connection
/// is using or recovering the database.
pub(crate) fn recover_hot_journal(
    source: &Path,
) -> Result<Option<RecoveredSqliteDatabase>, String> {
    recover_hot_journal_with_flags(source, OpenFlags::empty(), None)
}

fn recover_hot_journal_with_flags(
    source: &Path,
    flags: OpenFlags,
    vfs: Option<&CStr>,
) -> Result<Option<RecoveredSqliteDatabase>, String> {
    let no_follow = flags.contains(OpenFlags::SQLITE_OPEN_NOFOLLOW);
    if !journal_is_hot(source, flags, vfs)? {
        return Ok(None);
    }
    let directory = tempfile::Builder::new()
        .prefix("ocm-sqlite-recovery-")
        .tempdir()
        .map_err(|error| format!("failed to create private SQLite recovery: {error}"))?;
    let copy = directory.path().join(RECOVERED_DATABASE);
    let Some(source_digest) = copy_database_files(source, &copy, no_follow)? else {
        return Ok(None);
    };
    // Lock state is not copied: a journal that stayed hot with the same bytes
    // proves no writer changed the database while it was read.
    if !journal_is_hot(source, flags, vfs)?
        || database_digest(source, no_follow)? != Some(source_digest)
    {
        return Ok(None);
    }
    // Replay of a multi-database transaction depends on its super-journal,
    // a file named inside the journal and outside this copy, which SQLite
    // consults and may delete. Recover only self-contained journals.
    if journal_names_super_journal(&journal_path(&copy))? {
        return Err(format!(
            "cannot recover SQLite database {}: its rollback journal belongs to an interrupted multi-database transaction; open the database with SQLite to recover it, then retry",
            source.display()
        ));
    }
    let recovered = Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|connection| {
        connection.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
        connection.close().map_err(|(_, error)| error)
    });
    recovered.map_err(|error| {
        format!(
            "failed to recover SQLite rollback journal for {}: {error}",
            source.display()
        )
    })?;
    Ok(Some(RecoveredSqliteDatabase {
        directory,
        source_digest,
    }))
}

fn journal_is_hot(source: &Path, flags: OpenFlags, vfs: Option<&CStr>) -> Result<bool, String> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | flags;
    let connection = if let Some(vfs) = vfs {
        Connection::open_with_flags_and_vfs(source, flags, vfs)
    } else {
        Connection::open_with_flags(source, flags)
    };
    match connection
        .and_then(|connection| connection.query_row("PRAGMA schema_version", [], |_| Ok(())))
    {
        Ok(()) => Ok(false),
        Err(error) if sqlite_needs_rollback(&error) => Ok(true),
        Err(error) if sqlite_is_busy(&error) => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect SQLite database {}: {error}",
            source.display()
        )),
    }
}

/// Whether a rollback journal ends with a super-journal record, the trailer
/// SQLite reads before replay: name length, checksum and the journal magic.
fn journal_names_super_journal(journal: &Path) -> Result<bool, String> {
    const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    let mut file = File::open(journal)
        .map_err(|error| format!("failed to open {}: {error}", journal.display()))?;
    let length = file
        .metadata()
        .map_err(|error| format!("failed to inspect {}: {error}", journal.display()))?
        .len();
    if length < 16 {
        return Ok(false);
    }
    let mut trailer = [0_u8; 16];
    file.seek(SeekFrom::Start(length - 16))
        .and_then(|_| file.read_exact(&mut trailer))
        .map_err(|error| format!("failed to read {}: {error}", journal.display()))?;
    let name_length = u32::from_be_bytes(trailer[..4].try_into().unwrap());
    Ok(trailer[8..] == JOURNAL_MAGIC && name_length != 0)
}

fn journal_path(database: &Path) -> PathBuf {
    let mut journal = database.as_os_str().to_os_string();
    journal.push("-journal");
    PathBuf::from(journal)
}

fn open_database_file(path: &Path, no_follow: bool) -> Result<Option<File>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    if no_follow {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    let _ = no_follow;
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to open {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "SQLite database file is not a regular file: {}",
            path.display()
        ));
    }
    Ok(Some(file))
}

/// Hashes the database and its journal, or returns `None` without a journal.
fn database_digest(database: &Path, no_follow: bool) -> Result<Option<[u8; 32]>, String> {
    copy_database_files_into(database, None, no_follow)
}

/// Copies the database and its journal into private files beside `copy`.
fn copy_database_files(
    source: &Path,
    copy: &Path,
    no_follow: bool,
) -> Result<Option<[u8; 32]>, String> {
    copy_database_files_into(source, Some(copy), no_follow)
}

fn copy_database_files_into(
    source: &Path,
    copy: Option<&Path>,
    no_follow: bool,
) -> Result<Option<[u8; 32]>, String> {
    let mut digest = Sha256::new();
    for (path, target) in [
        (source.to_path_buf(), copy.map(Path::to_path_buf)),
        (journal_path(source), copy.map(journal_path)),
    ] {
        let Some(mut file) = open_database_file(&path, no_follow)? else {
            return Ok(None);
        };
        let mut target = target
            .map(|target| {
                create_private_file(&target).map_err(|error| {
                    format!("failed to create private SQLite recovery file: {error}")
                })
            })
            .transpose()?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            if let Some(target) = &mut target {
                target
                    .write_all(&buffer[..read])
                    .map_err(|error| format!("failed to copy {}: {error}", path.display()))?;
            }
        }
        digest.update(hasher.finalize());
    }
    Ok(Some(digest.finalize().into()))
}

fn create_private_file(path: &Path) -> std::io::Result<File> {
    #[cfg(target_os = "macos")]
    return crate::infra::macos_security::create_private_file_new(path);
    #[cfg(windows)]
    return crate::infra::windows_security::create_private_file_new(path);
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
}

pub(crate) fn create_sqlite_snapshot(source: &Path) -> Result<SqliteSnapshot, String> {
    create_sqlite_snapshot_with_flags(source, OpenFlags::empty(), None)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn create_sqlite_snapshot_no_follow(source: &Path) -> Result<SqliteSnapshot, String> {
    create_sqlite_snapshot_with_flags(source, OpenFlags::SQLITE_OPEN_NOFOLLOW, None)
}

#[cfg(target_os = "linux")]
pub(crate) fn create_sqlite_snapshot_in_directory(
    directory: &File,
    leaf: &std::ffi::OsStr,
) -> Result<SqliteSnapshot, String> {
    use std::os::fd::AsRawFd;
    let path =
        std::path::PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(leaf);
    create_sqlite_snapshot_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_NOFOLLOW,
        Some(super::sqlite_directory_vfs::pinned_directory_vfs()?),
    )
}

fn create_sqlite_snapshot_with_flags(
    source: &Path,
    flags: OpenFlags,
    vfs: Option<&CStr>,
) -> Result<SqliteSnapshot, String> {
    #[cfg(target_os = "macos")]
    let file = tempfile::Builder::new().make(crate::infra::macos_security::create_private_file_new);
    #[cfg(windows)]
    let file =
        tempfile::Builder::new().make(crate::infra::windows_security::create_private_file_new);
    #[cfg(not(any(windows, target_os = "macos")))]
    let file = NamedTempFile::new();
    let snapshot = SqliteSnapshot {
        file: file.map_err(|error| format!("failed to create private SQLite snapshot: {error}"))?,
    };
    create_sqlite_snapshot_inner(source, snapshot.path(), flags, vfs)?;
    // Flush through the owned writable handle; Windows rejects a read-only handle.
    snapshot.file.as_file().sync_all().map_err(|error| {
        format!(
            "failed to sync SQLite snapshot {}: {error}",
            snapshot.path().display()
        )
    })?;
    Ok(snapshot)
}

fn create_sqlite_snapshot_inner(
    source: &Path,
    target: &Path,
    flags: OpenFlags,
    vfs: Option<&CStr>,
) -> Result<(), String> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | flags;
    let mut target_db = Connection::open_with_flags(
        target,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        format!(
            "failed to create SQLite snapshot {}: {error}",
            target.display()
        )
    })?;
    target_db
        .busy_timeout(SQLITE_BACKUP_RETRY_DELAY)
        .map_err(|error| {
            format!(
                "failed to configure SQLite snapshot target {}: {error}",
                target.display()
            )
        })?;

    let deadline = Instant::now() + SQLITE_BACKUP_TIMEOUT;
    loop {
        let source_db = open_snapshot_source(source, source, flags, vfs)?;
        if backup_database(source, &source_db, &mut target_db, deadline)? {
            close_snapshot_source(source_db)?;
            break;
        }
        drop(source_db);
        // A read-only connection cannot replay a hot rollback journal; copy
        // the committed state SQLite recovers from it instead.
        if let Some(recovered) = recover_hot_journal_with_flags(source, flags, vfs)? {
            let recovered_db = open_snapshot_source(
                &recovered.path(),
                source,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                None,
            )?;
            if !backup_database(source, &recovered_db, &mut target_db, deadline)? {
                return Err(format!(
                    "failed to recover SQLite rollback journal for {}",
                    source.display()
                ));
            }
            close_snapshot_source(recovered_db)?;
            break;
        }
        // Another connection is using or recovering the journal; retry it.
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out snapshotting SQLite database {}",
                source.display()
            ));
        }
        sleep(SQLITE_BACKUP_RETRY_DELAY);
    }

    let mut quick_check = target_db
        .prepare("PRAGMA quick_check")
        .map_err(|error| format!("failed to verify SQLite snapshot: {error}"))?;
    let rows = quick_check
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("failed to verify SQLite snapshot: {error}"))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|error| format!("failed to verify SQLite snapshot: {error}"))?);
    }
    if results.as_slice() != ["ok"] {
        return Err(format!(
            "SQLite snapshot integrity check failed for {}: {}",
            source.display(),
            results.join("; ")
        ));
    }
    drop(quick_check);
    target_db
        .close()
        .map_err(|(_, error)| format!("failed to close SQLite snapshot: {error}"))?;

    Ok(())
}

fn open_snapshot_source(
    path: &Path,
    source: &Path,
    flags: OpenFlags,
    vfs: Option<&CStr>,
) -> Result<Connection, String> {
    let connection = if let Some(vfs) = vfs {
        Connection::open_with_flags_and_vfs(path, flags, vfs)
    } else {
        Connection::open_with_flags(path, flags)
    }
    .map_err(|error| {
        format!(
            "failed to open SQLite database {} for snapshot: {error}",
            source.display()
        )
    })?;
    connection
        .busy_timeout(SQLITE_BACKUP_RETRY_DELAY)
        .map_err(|error| {
            format!(
                "failed to configure SQLite snapshot timeout for {}: {error}",
                source.display()
            )
        })?;
    Ok(connection)
}

fn close_snapshot_source(connection: Connection) -> Result<(), String> {
    connection
        .close()
        .map_err(|(_, error)| format!("failed to close SQLite source: {error}"))
}

/// Copies `source_db` into `target_db`. Returns `false`, having copied nothing,
/// when the source has a hot rollback journal.
fn backup_database(
    source: &Path,
    source_db: &Connection,
    target_db: &mut Connection,
    deadline: Instant,
) -> Result<bool, String> {
    let backup = Backup::new(source_db, target_db).map_err(|error| {
        format!(
            "failed to initialize SQLite snapshot for {}: {error}",
            source.display()
        )
    })?;
    loop {
        let status = match backup.step(SQLITE_BACKUP_PAGES_PER_STEP) {
            Ok(status) => status,
            Err(error) if sqlite_needs_rollback(&error) => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "failed to copy SQLite database {}: {error}",
                    source.display()
                ));
            }
        };
        match status {
            StepResult::Done => return Ok(true),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => sleep(SQLITE_BACKUP_RETRY_DELAY),
            _ => {
                return Err(format!(
                    "SQLite backup returned an unsupported status for {}",
                    source.display()
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out snapshotting SQLite database {}",
                source.display()
            ));
        }
    }
}

/// Leaves `database` with a hot rollback journal over 300 committed rows, as a
/// writer killed mid-transaction does: the uncommitted change is spilled into
/// the database file and its original pages are in the journal. Returns the
/// byte offset of a page the transaction never touched, so the journal does
/// not restore it.
#[cfg(all(test, unix))]
pub(crate) fn plant_hot_journal(database: &Path) -> u64 {
    let scratch = tempfile::tempdir().unwrap();
    let writer_path = scratch.path().join("writer.sqlite");
    let writer = Connection::open(&writer_path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 299)
             INSERT INTO records(value) SELECT 'committed' || i FROM n;
             CREATE TABLE untouched(value TEXT NOT NULL);
             WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 399)
             INSERT INTO untouched SELECT printf('%.300c', 'u') FROM n;
             PRAGMA cache_size=5;
             BEGIN;
             UPDATE records SET value = value || printf('%.2000c', 'x');
             INSERT INTO records(value) SELECT value FROM records;",
        )
        .unwrap();
    let untouched_page = writer
        .query_row(
            "SELECT (rootpage - 1) * (SELECT page_size FROM pragma_page_size())
             FROM sqlite_schema WHERE name = 'untouched'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let journal = journal_path(&writer_path);
    assert!(journal.exists(), "the writer did not spill its transaction");
    // Copies carry no locks, so the copied journal is hot.
    std::fs::copy(&writer_path, database).unwrap();
    std::fs::copy(&journal, journal_path(database)).unwrap();
    u64::try_from(untouched_page).unwrap()
}

/// Appends the super-journal record of a multi-database transaction naming
/// `super_journal`, as SQLite writes it at the end of each child journal.
#[cfg(all(test, unix))]
pub(crate) fn name_super_journal(database: &Path, super_journal: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let name = super_journal.as_os_str().as_bytes();
    let checksum = name
        .iter()
        .fold(0_u32, |sum, byte| sum.wrapping_add(u32::from(*byte)));
    let mut record = 0x4000_0001_u32.to_be_bytes().to_vec();
    record.extend_from_slice(name);
    record.extend_from_slice(&u32::try_from(name.len()).unwrap().to_be_bytes());
    record.extend_from_slice(&checksum.to_be_bytes());
    record.extend_from_slice(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]);
    std::fs::OpenOptions::new()
        .append(true)
        .open(journal_path(database))
        .unwrap()
        .write_all(&record)
        .unwrap();
}

/// Overwrites the start of the page at `offset` so integrity checks fail.
#[cfg(all(test, unix))]
pub(crate) fn damage_page(database: &Path, offset: u64) {
    use std::os::unix::fs::FileExt;
    std::fs::OpenOptions::new()
        .write(true)
        .open(database)
        .unwrap()
        .write_all_at(&[0xff; 64], offset)
        .unwrap();
}

/// The committed rows a read-write open recovers from `database` and its journal.
#[cfg(all(test, unix))]
pub(crate) fn recovered_records(database: &Path) -> Vec<String> {
    let scratch = tempfile::tempdir().unwrap();
    let copy = scratch.path().join("copy.sqlite");
    std::fs::copy(database, &copy).unwrap();
    if journal_path(database).exists() {
        std::fs::copy(journal_path(database), journal_path(&copy)).unwrap();
    }
    let connection = Connection::open(&copy).unwrap();
    let records = connection
        .prepare("SELECT value FROM records ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(target_os = "linux")]
    #[test]
    fn sqlite_directory_snapshot_retains_committed_wal_after_parent_replacement() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        fs::create_dir(&source).unwrap();
        let mut writer = Connection::open(source.join("history.db")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(value TEXT); INSERT INTO records VALUES ('committed');").unwrap();
        let transaction = writer.transaction().unwrap();
        transaction
            .execute("INSERT INTO records VALUES ('pending')", [])
            .unwrap();
        let directory = File::open(&source).unwrap();
        let saved = fixture.path().join("saved");
        fs::rename(&source, &saved).unwrap();
        fs::create_dir(&source).unwrap();
        let other = Connection::open(source.join("history.db")).unwrap();
        other
            .execute_batch(
                "CREATE TABLE records(value TEXT); INSERT INTO records VALUES ('outside');",
            )
            .unwrap();
        let snapshot =
            create_sqlite_snapshot_in_directory(&directory, std::ffi::OsStr::new("history.db"))
                .unwrap();
        let db = Connection::open(snapshot.path()).unwrap();
        let values: Vec<String> = db
            .prepare("SELECT value FROM records")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(values, ["committed"]);
        drop(other);
        fs::remove_dir_all(&source).unwrap();
        fs::rename(&saved, &source).unwrap();
        drop(transaction);
        drop(writer);
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_snapshot_is_private_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;

        let source = NamedTempFile::new().unwrap();
        let connection = Connection::open(source.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE durable_state (value TEXT NOT NULL);
                 INSERT INTO durable_state VALUES ('private');",
            )
            .unwrap();
        drop(connection);

        let snapshot = create_sqlite_snapshot(source.path()).unwrap();
        let snapshot_path = snapshot.path().to_path_buf();
        let mode = fs::metadata(&snapshot_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        drop(snapshot);
        assert!(!snapshot_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_snapshot_captures_committed_rows_behind_hot_journal() {
        let source = tempfile::tempdir().unwrap();
        let database = source.path().join("app.db");
        let _ = plant_hot_journal(&database);
        let committed = (0..300)
            .map(|i| format!("committed{i}"))
            .collect::<Vec<_>>();
        assert_eq!(recovered_records(&database), committed);
        let before = [
            fs::read(&database).unwrap(),
            fs::read(journal_path(&database)).unwrap(),
        ];
        let read_only = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("SELECT count(*) FROM records", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_err();
        assert!(sqlite_needs_rollback(&read_only), "{read_only}");

        let snapshot = create_sqlite_snapshot(&database).unwrap();

        assert_eq!(recovered_records(snapshot.path()), committed);
        assert!(!journal_path(snapshot.path()).exists());
        assert_eq!(
            [
                fs::read(&database).unwrap(),
                fs::read(journal_path(&database)).unwrap()
            ],
            before
        );
        let mut entries = fs::read_dir(source.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, ["app.db", "app.db-journal"]);
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_snapshot_refuses_a_corrupt_database_behind_hot_journal() {
        let source = tempfile::tempdir().unwrap();
        let database = source.path().join("app.db");
        let untouched = plant_hot_journal(&database);
        damage_page(&database, untouched);
        let before = [
            fs::read(&database).unwrap(),
            fs::read(journal_path(&database)).unwrap(),
        ];

        let error = create_sqlite_snapshot(&database).err().unwrap();

        assert!(
            error.contains("integrity check failed") || error.contains("malformed"),
            "{error}"
        );
        assert_eq!(
            [
                fs::read(&database).unwrap(),
                fs::read(journal_path(&database)).unwrap()
            ],
            before
        );
    }

    #[cfg(unix)]
    #[test]
    fn hot_journal_recovery_refuses_a_multi_database_transaction() {
        let source = tempfile::tempdir().unwrap();
        let database = source.path().join("app.db");
        let _ = plant_hot_journal(&database);
        let outside = tempfile::tempdir().unwrap();
        let super_journal = outside.path().join("transaction-mj");
        fs::write(&super_journal, b"").unwrap();
        name_super_journal(&database, &super_journal);
        let before = [
            fs::read(&database).unwrap(),
            fs::read(journal_path(&database)).unwrap(),
        ];

        let error = recover_hot_journal(&database).err().unwrap();
        assert!(error.contains("multi-database transaction"), "{error}");
        let error = create_sqlite_snapshot(&database).err().unwrap();
        assert!(error.contains("multi-database transaction"), "{error}");

        assert!(super_journal.exists());
        assert_eq!(
            [
                fs::read(&database).unwrap(),
                fs::read(journal_path(&database)).unwrap()
            ],
            before
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sqlite_directory_snapshot_captures_committed_rows_behind_hot_journal() {
        let source = tempfile::tempdir().unwrap();
        let _ = plant_hot_journal(&source.path().join("app.db"));
        let directory = File::open(source.path()).unwrap();

        let snapshot =
            create_sqlite_snapshot_in_directory(&directory, std::ffi::OsStr::new("app.db"))
                .unwrap();

        assert_eq!(
            recovered_records(snapshot.path()),
            (0..300)
                .map(|i| format!("committed{i}"))
                .collect::<Vec<_>>()
        );
        assert!(journal_path(&source.path().join("app.db")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn hot_journal_recovery_refuses_a_database_in_use() {
        let source = tempfile::tempdir().unwrap();
        let database = source.path().join("app.db");
        let _ = plant_hot_journal(&database);
        let recovered = recover_hot_journal(&database).unwrap().unwrap();
        assert!(recovered.matches(&database).unwrap());

        // A writer recovers the journal and holds a new transaction.
        let writer = Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "PRAGMA cache_size=5;
                 BEGIN;
                 UPDATE records SET value = value || printf('%.2000c', 'y');",
            )
            .unwrap();
        assert!(recover_hot_journal(&database).unwrap().is_none());
        assert!(!recovered.matches(&database).unwrap());
        drop(writer);
        assert!(recover_hot_journal(&database).unwrap().is_none());
    }
}
