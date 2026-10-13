use std::fs::File;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use tempfile::NamedTempFile;

const SQLITE_BACKUP_PAGES_PER_STEP: i32 = 256;
const SQLITE_BACKUP_RETRY_DELAY: Duration = Duration::from_millis(10);
const SQLITE_BACKUP_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct SqliteSnapshot {
    file: NamedTempFile,
}

impl SqliteSnapshot {
    pub(crate) fn path(&self) -> &Path {
        self.file.path()
    }
}

pub(crate) fn create_sqlite_snapshot(source: &Path) -> Result<SqliteSnapshot, String> {
    let snapshot = SqliteSnapshot {
        file: NamedTempFile::new()
            .map_err(|error| format!("failed to create private SQLite snapshot: {error}"))?,
    };
    create_sqlite_snapshot_inner(source, snapshot.path())?;
    Ok(snapshot)
}

fn create_sqlite_snapshot_inner(source: &Path, target: &Path) -> Result<(), String> {
    let source_db = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        format!(
            "failed to open SQLite database {} for snapshot: {error}",
            source.display()
        )
    })?;
    source_db
        .busy_timeout(SQLITE_BACKUP_RETRY_DELAY)
        .map_err(|error| {
            format!(
                "failed to configure SQLite snapshot timeout for {}: {error}",
                source.display()
            )
        })?;

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

    let backup = Backup::new(&source_db, &mut target_db).map_err(|error| {
        format!(
            "failed to initialize SQLite snapshot for {}: {error}",
            source.display()
        )
    })?;
    let deadline = Instant::now() + SQLITE_BACKUP_TIMEOUT;
    loop {
        let status = backup.step(SQLITE_BACKUP_PAGES_PER_STEP).map_err(|error| {
            format!(
                "failed to copy SQLite database {}: {error}",
                source.display()
            )
        })?;
        match status {
            StepResult::Done => break,
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
    drop(backup);

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
    source_db
        .close()
        .map_err(|(_, error)| format!("failed to close SQLite source: {error}"))?;

    File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            format!(
                "failed to sync SQLite snapshot {}: {error}",
                target.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
}
