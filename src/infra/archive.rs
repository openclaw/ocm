use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tar::{Archive, Builder, Header};
use time::OffsetDateTime;
use zip::ZipArchive;

use crate::env::{default_service_enabled, default_service_running};
use crate::infra::sqlite_snapshot::create_sqlite_snapshot;

pub const ENV_ARCHIVE_METADATA_PATH: &str = "meta/env.json";
pub const ENV_ARCHIVE_ROOT_DIR: &str = "root";
const MAX_ENV_ARCHIVE_WRITE_ATTEMPTS: usize = 2;
const SQLITE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

enum EnvArchiveWriteError {
    EntryDisappeared(String),
    Fatal(String),
}

impl EnvArchiveWriteError {
    fn from_entry_io(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::EntryDisappeared(error.to_string())
        } else {
            Self::Fatal(error.to_string())
        }
    }

    fn into_message(self) -> String {
        match self {
            Self::EntryDisappeared(message) | Self::Fatal(message) => message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedEnvMeta {
    pub name: String,
    #[serde(default)]
    pub source_root: Option<String>,
    pub gateway_port: Option<u32>,
    #[serde(default)]
    pub gateway_port_auto_assigned: bool,
    #[serde(default = "default_service_enabled")]
    pub service_enabled: bool,
    #[serde(default = "default_service_running")]
    pub service_running: bool,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub protected: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvArchiveMetadata {
    pub kind: String,
    pub format_version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub exported_at: OffsetDateTime,
    pub env: ArchivedEnvMeta,
}

pub struct ExtractedEnvArchive<T> {
    pub metadata: T,
    pub root_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvArchiveEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Clone)]
pub struct EnvArchiveOptions {
    pub should_skip_path: fn(&Path, EnvArchiveEntryKind) -> bool,
    pub included_path_roots: BTreeSet<PathBuf>,
    pub excluded_path_roots: BTreeSet<PathBuf>,
    pub snapshot_sqlite_files: bool,
}

impl Default for EnvArchiveOptions {
    fn default() -> Self {
        Self {
            should_skip_path: include_env_archive_path,
            included_path_roots: BTreeSet::new(),
            excluded_path_roots: BTreeSet::new(),
            snapshot_sqlite_files: false,
        }
    }
}

impl EnvArchiveOptions {
    fn should_skip(&self, relative_path: &Path, kind: EnvArchiveEntryKind) -> bool {
        if self
            .excluded_path_roots
            .iter()
            .any(|root| relative_path == root || relative_path.starts_with(root))
        {
            return true;
        }
        let explicitly_included = self.included_path_roots.iter().any(|root| {
            relative_path == root
                || relative_path.starts_with(root)
                || root.starts_with(relative_path)
        });
        !explicitly_included && (self.should_skip_path)(relative_path, kind)
    }
}

fn include_env_archive_path(_relative_path: &Path, _kind: EnvArchiveEntryKind) -> bool {
    false
}

pub fn write_env_archive<T: Serialize>(
    metadata: &T,
    source_root: &Path,
    output_path: &Path,
) -> Result<(), String> {
    write_env_archive_with_options(
        metadata,
        source_root,
        output_path,
        EnvArchiveOptions::default(),
    )
}

pub fn write_env_archive_with_options<T: Serialize>(
    metadata: &T,
    source_root: &Path,
    output_path: &Path,
    options: EnvArchiveOptions,
) -> Result<(), String> {
    for attempt in 0..MAX_ENV_ARCHIVE_WRITE_ATTEMPTS {
        match write_env_archive_attempt(metadata, source_root, output_path, options.clone()) {
            Ok(()) => return Ok(()),
            Err(EnvArchiveWriteError::EntryDisappeared(_))
                if attempt + 1 < MAX_ENV_ARCHIVE_WRITE_ATTEMPTS => {}
            Err(error) => return Err(error.into_message()),
        }
    }
    unreachable!("archive write attempts are non-zero")
}

fn write_env_archive_attempt<T: Serialize>(
    metadata: &T,
    source_root: &Path,
    output_path: &Path,
    options: EnvArchiveOptions,
) -> Result<(), EnvArchiveWriteError> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
    }

    let file = File::create(output_path)
        .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
    let mut builder = Builder::new(file);
    builder.follow_symlinks(false);
    let mut metadata_raw = serde_json::to_string_pretty(metadata)
        .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
    metadata_raw.push('\n');
    let metadata_bytes = metadata_raw.into_bytes();
    let mut header = Header::new_gnu();
    header.set_size(metadata_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            ENV_ARCHIVE_METADATA_PATH,
            Cursor::new(metadata_bytes),
        )
        .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
    append_env_root(&mut builder, source_root, &options)?;
    builder
        .finish()
        .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))
}

fn append_env_root(
    builder: &mut Builder<File>,
    source_root: &Path,
    options: &EnvArchiveOptions,
) -> Result<(), EnvArchiveWriteError> {
    builder
        .append_dir(ENV_ARCHIVE_ROOT_DIR, source_root)
        .map_err(|error| {
            EnvArchiveWriteError::Fatal(format!(
                "failed to archive {}: {error}",
                source_root.display()
            ))
        })?;

    let mut stack = sorted_child_paths(source_root)
        .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
    while let Some(path) = stack.pop() {
        let relative_path = path.strip_prefix(source_root).map_err(|error| {
            EnvArchiveWriteError::Fatal(format!(
                "failed to resolve archive path for {}: {error}",
                path.display()
            ))
        })?;
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            EnvArchiveWriteError::from_entry_io(io_error_with_context(
                error,
                format!("failed to inspect {} for archive", path.display()),
            ))
        })?;
        let entry_kind = env_archive_entry_kind(&metadata);
        if options.should_skip(relative_path, entry_kind) {
            continue;
        }
        if options.snapshot_sqlite_files
            && sqlite_database_path_for_sidecar(relative_path).is_some()
        {
            continue;
        }

        let archive_path = Path::new(ENV_ARCHIVE_ROOT_DIR).join(relative_path);
        if metadata.is_dir() {
            builder.append_dir(&archive_path, &path).map_err(|error| {
                EnvArchiveWriteError::from_entry_io(io_error_with_context(
                    error,
                    format!("failed to archive {}", path.display()),
                ))
            })?;
            let mut children =
                sorted_child_paths(&path).map_err(EnvArchiveWriteError::from_entry_io)?;
            stack.append(&mut children);
            continue;
        }

        if options.snapshot_sqlite_files && is_sqlite_database_path(relative_path) {
            let source_metadata = fs::metadata(&path).map_err(|error| {
                EnvArchiveWriteError::from_entry_io(io_error_with_context(
                    error,
                    format!("failed to inspect SQLite database {}", path.display()),
                ))
            })?;
            if !source_metadata.is_file() {
                return Err(EnvArchiveWriteError::Fatal(format!(
                    "SQLite archive source is not a regular file: {}",
                    path.display()
                )));
            }
            let snapshot = create_sqlite_snapshot(&path).map_err(EnvArchiveWriteError::Fatal)?;
            let snapshot_metadata = fs::metadata(snapshot.path()).map_err(|error| {
                EnvArchiveWriteError::Fatal(format!(
                    "failed to inspect SQLite snapshot for {}: {error}",
                    path.display()
                ))
            })?;
            let mut header = Header::new_gnu();
            header.set_metadata(&source_metadata);
            header.set_size(snapshot_metadata.len());
            header.set_cksum();
            let mut snapshot_file = File::open(snapshot.path()).map_err(|error| {
                EnvArchiveWriteError::Fatal(format!(
                    "failed to open SQLite snapshot for {}: {error}",
                    path.display()
                ))
            })?;
            builder
                .append_data(&mut header, &archive_path, &mut snapshot_file)
                .map_err(|error| {
                    EnvArchiveWriteError::Fatal(format!(
                        "failed to archive SQLite database {}: {error}",
                        path.display()
                    ))
                })?;
            continue;
        }

        builder
            .append_path_with_name(&path, &archive_path)
            .map_err(|error| {
                EnvArchiveWriteError::from_entry_io(io_error_with_context(
                    error,
                    format!("failed to archive {}", path.display()),
                ))
            })?;
    }

    Ok(())
}

fn is_sqlite_database_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".sqlite"))
}

fn sqlite_database_path_for_sidecar(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    for suffix in SQLITE_SIDECAR_SUFFIXES {
        if let Some(database_name) = name.strip_suffix(suffix)
            && database_name.ends_with(".sqlite")
        {
            return Some(path.with_file_name(database_name));
        }
    }
    None
}

fn sorted_child_paths(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut children = fs::read_dir(path)
        .map_err(|error| {
            io_error_with_context(
                error,
                format!("failed to read directory {} for archive", path.display()),
            )
        })?
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                io_error_with_context(
                    error,
                    format!(
                        "failed to read a directory entry in {} for archive",
                        path.display()
                    ),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    children.sort();
    children.reverse();
    Ok(children)
}

fn io_error_with_context(error: io::Error, context: String) -> io::Error {
    io::Error::new(error.kind(), format!("{context}: {error}"))
}

fn env_archive_entry_kind(metadata: &fs::Metadata) -> EnvArchiveEntryKind {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        EnvArchiveEntryKind::Directory
    } else if file_type.is_file() {
        EnvArchiveEntryKind::File
    } else if file_type.is_symlink() {
        EnvArchiveEntryKind::Symlink
    } else {
        EnvArchiveEntryKind::Other
    }
}

pub fn extract_env_archive<T: DeserializeOwned>(
    archive_path: &Path,
    staging_dir: &Path,
) -> Result<ExtractedEnvArchive<T>, String> {
    fs::create_dir_all(staging_dir).map_err(|error| error.to_string())?;

    let file = File::open(archive_path).map_err(|error| error.to_string())?;
    let mut archive = Archive::new(file);
    archive
        .unpack(staging_dir)
        .map_err(|error| error.to_string())?;

    let metadata_path = staging_dir.join(ENV_ARCHIVE_METADATA_PATH);
    let root_dir = staging_dir.join(ENV_ARCHIVE_ROOT_DIR);
    // Validate parents before inspecting children; archive symlinks may point
    // outside staging even though tar safely contained their creation.
    for (relative_path, directory) in [
        ("meta", true),
        (ENV_ARCHIVE_METADATA_PATH, false),
        (ENV_ARCHIVE_ROOT_DIR, true),
    ] {
        let metadata = fs::symlink_metadata(staging_dir.join(relative_path)).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                format!("archive is missing {relative_path}")
            } else {
                format!("failed to inspect archive entry {relative_path}: {error}")
            }
        })?;
        let valid = if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if !valid {
            let expected = if directory {
                "directory"
            } else {
                "regular file"
            };
            return Err(format!(
                "archive entry {relative_path} must be a {expected}, not a symlink or other file type"
            ));
        }
    }

    let metadata_raw = fs::read_to_string(&metadata_path).map_err(|error| error.to_string())?;
    let metadata = serde_json::from_str(&metadata_raw).map_err(|error| error.to_string())?;
    Ok(ExtractedEnvArchive { metadata, root_dir })
}

pub fn extract_tar_gz(archive_path: &Path, destination_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(destination_dir).map_err(|error| error.to_string())?;

    let file = File::open(archive_path).map_err(|error| error.to_string())?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(destination_dir)
        .map_err(|error| error.to_string())
}

pub fn extract_zip(archive_path: &Path, destination_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(destination_dir).map_err(|error| error.to_string())?;

    let file = File::open(archive_path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let Some(relative_path) = entry.enclosed_name() else {
            continue;
        };
        let output_path = destination_dir.join(relative_path);
        if entry.is_dir() {
            fs::create_dir_all(&output_path).map_err(|error| error.to_string())?;
            continue;
        }

        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }

        let mut output = File::create(&output_path).map_err(|error| error.to_string())?;
        io::copy(&mut entry, &mut output).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = output
                .metadata()
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(mode);
            fs::set_permissions(&output_path, permissions).map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };
    use std::{fs, io};

    use rusqlite::{Connection, OpenFlags};

    use super::{
        ArchivedEnvMeta, EnvArchiveEntryKind, EnvArchiveMetadata, EnvArchiveOptions,
        EnvArchiveWriteError, extract_env_archive, write_env_archive,
        write_env_archive_with_options,
    };

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    static DISAPPEARING_TEST_LOCK: Mutex<()> = Mutex::new(());
    static PATHS_TO_REMOVE_BEFORE_ARCHIVE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    fn temp_path(label: &str) -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join("ocm-archive-tests")
            .join(format!("{label}-{}-{id}", std::process::id()))
    }

    #[cfg(unix)]
    #[test]
    fn env_archives_preserve_symlinks_inside_the_root() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("data.txt"), "fixture data").unwrap();
        std::os::unix::fs::symlink("data.txt", source.join("alias.txt")).unwrap();
        let archive = fixture.path().join("env.tar");
        write_env_archive(&serde_json::json!({"fixture": true}), &source, &archive).unwrap();

        let extracted =
            extract_env_archive::<serde_json::Value>(&archive, &fixture.path().join("extracted"))
                .unwrap();
        assert_eq!(
            fs::read_link(extracted.root_dir.join("alias.txt")).unwrap(),
            Path::new("data.txt")
        );
        assert_eq!(
            fs::read_to_string(extracted.root_dir.join("alias.txt")).unwrap(),
            "fixture data"
        );
    }

    fn remove_configured_path_before_archive(
        relative_path: &Path,
        _kind: EnvArchiveEntryKind,
    ) -> bool {
        let mut paths = PATHS_TO_REMOVE_BEFORE_ARCHIVE.lock().unwrap();
        if let Some(index) = paths.iter().position(|path| path.ends_with(relative_path)) {
            let path = paths.remove(index);
            fs::remove_file(&path).unwrap();
            if relative_path == Path::new("transient.db-shm") {
                fs::write(path.with_file_name("stable.txt"), "contents after retry").unwrap();
            }
        }
        false
    }

    fn skip_openclaw_paths(relative_path: &Path, _kind: EnvArchiveEntryKind) -> bool {
        relative_path.starts_with(".openclaw")
    }

    #[test]
    fn archive_options_include_only_the_selected_path_branch() {
        let options = EnvArchiveOptions {
            should_skip_path: skip_openclaw_paths,
            included_path_roots: BTreeSet::from([PathBuf::from(".openclaw/team/ops")]),
            excluded_path_roots: BTreeSet::new(),
            snapshot_sqlite_files: false,
        };

        for path in [
            ".openclaw",
            ".openclaw/team",
            ".openclaw/team/ops",
            ".openclaw/team/ops/notes.md",
        ] {
            assert!(
                !options.should_skip(Path::new(path), EnvArchiveEntryKind::File),
                "{path} should be traversed"
            );
        }
        assert!(options.should_skip(
            Path::new(".openclaw/team/cache"),
            EnvArchiveEntryKind::Directory
        ));
    }

    #[test]
    fn archive_options_exclusions_override_selected_path_branches() {
        let options = EnvArchiveOptions {
            should_skip_path: skip_openclaw_paths,
            included_path_roots: BTreeSet::from([PathBuf::from(".openclaw/extensions")]),
            excluded_path_roots: BTreeSet::from([PathBuf::from(
                ".openclaw/extensions/demo/node_modules",
            )]),
            snapshot_sqlite_files: false,
        };

        assert!(!options.should_skip(
            Path::new(".openclaw/extensions/demo/package.json"),
            EnvArchiveEntryKind::File
        ));
        assert!(options.should_skip(
            Path::new(".openclaw/extensions/demo/node_modules/package/index.js"),
            EnvArchiveEntryKind::File
        ));
    }

    #[test]
    fn env_archive_retries_when_entry_disappears_before_archive_insertion() {
        let _test_guard = DISAPPEARING_TEST_LOCK.lock().unwrap();
        let source_root = temp_path("disappearing-source-root");
        let archive_path = temp_path("disappearing-archives").join("demo.ocm-env.tar");
        let extract_dir = temp_path("disappearing-extract");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(source_root.join("stable.txt"), "stable contents").unwrap();
        let transient_path = source_root.join("transient.db-shm");
        fs::write(&transient_path, "transient contents").unwrap();
        *PATHS_TO_REMOVE_BEFORE_ARCHIVE.lock().unwrap() = vec![transient_path];

        write_env_archive_with_options(
            &serde_json::json!({ "kind": "test-archive" }),
            &source_root,
            &archive_path,
            EnvArchiveOptions {
                should_skip_path: remove_configured_path_before_archive,
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap();

        let extracted =
            extract_env_archive::<serde_json::Value>(&archive_path, &extract_dir).unwrap();
        assert_eq!(
            fs::read_to_string(extracted.root_dir.join("stable.txt")).unwrap(),
            "contents after retry"
        );
        assert!(!extracted.root_dir.join("transient.db-shm").exists());

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(extract_dir);
        let _ = fs::remove_file(archive_path);
    }

    #[test]
    fn env_archive_keeps_repeated_disappearances_fatal() {
        let _test_guard = DISAPPEARING_TEST_LOCK.lock().unwrap();
        let source_root = temp_path("repeated-disappearing-source-root");
        let archive_path = temp_path("repeated-disappearing-archives").join("demo.ocm-env.tar");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(source_root.join("stable.txt"), "stable contents").unwrap();
        let first_transient = source_root.join("transient-a.db-shm");
        let second_transient = source_root.join("transient-b.db-shm");
        fs::write(&first_transient, "first transient contents").unwrap();
        fs::write(&second_transient, "second transient contents").unwrap();
        // One configured disappearance is consumed per archive attempt.
        *PATHS_TO_REMOVE_BEFORE_ARCHIVE.lock().unwrap() = vec![first_transient, second_transient];

        let error = write_env_archive_with_options(
            &serde_json::json!({ "kind": "test-archive" }),
            &source_root,
            &archive_path,
            EnvArchiveOptions {
                should_skip_path: remove_configured_path_before_archive,
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap_err();

        assert!(error.contains("failed to archive"), "{error}");
        assert!(PATHS_TO_REMOVE_BEFORE_ARCHIVE.lock().unwrap().is_empty());

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_file(archive_path);
    }

    #[test]
    fn archive_entry_errors_other_than_not_found_are_fatal() {
        let error = EnvArchiveWriteError::from_entry_io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "permission denied",
        ));

        assert!(matches!(error, EnvArchiveWriteError::Fatal(_)));
    }

    #[test]
    fn env_archive_snapshots_live_sqlite_state_without_sidecars() {
        let source_root = temp_path("sqlite-source-root");
        let archive_path = temp_path("sqlite-archives").join("demo.ocm-env.tar");
        let extract_dir = temp_path("sqlite-extract");
        let database_path = source_root.join(".openclaw/state/openclaw.sqlite");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();

        let connection = Connection::open(&database_path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&database_path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE durable_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO durable_state VALUES ('sentinel', 'must survive');",
            )
            .unwrap();
        assert!(database_path.with_extension("sqlite-wal").exists());

        write_env_archive_with_options(
            &serde_json::json!({ "kind": "test-archive" }),
            &source_root,
            &archive_path,
            EnvArchiveOptions {
                snapshot_sqlite_files: true,
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap();

        let extracted =
            extract_env_archive::<serde_json::Value>(&archive_path, &extract_dir).unwrap();
        let restored_database = extracted.root_dir.join(".openclaw/state/openclaw.sqlite");
        assert!(restored_database.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&restored_database)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o640);
        }
        assert!(!restored_database.with_extension("sqlite-wal").exists());
        assert!(!restored_database.with_extension("sqlite-shm").exists());
        assert!(!restored_database.with_extension("sqlite-journal").exists());

        let restored = Connection::open_with_flags(
            restored_database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let value: String = restored
            .query_row(
                "SELECT value FROM durable_state WHERE key = 'sentinel'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "must survive");

        drop(restored);
        drop(connection);
        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(extract_dir);
        let _ = fs::remove_file(archive_path);
    }

    #[test]
    fn env_archive_refuses_to_raw_copy_malformed_sqlite_state() {
        let source_root = temp_path("malformed-sqlite-source-root");
        let archive_path = temp_path("malformed-sqlite-archives").join("demo.ocm-env.tar");
        let database_path = source_root.join(".openclaw/state/openclaw.sqlite");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        fs::write(&database_path, "not a sqlite database").unwrap();

        let error = write_env_archive_with_options(
            &serde_json::json!({ "kind": "test-archive" }),
            &source_root,
            &archive_path,
            EnvArchiveOptions {
                snapshot_sqlite_files: true,
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap_err();

        assert!(error.contains("SQLite"), "{error}");
        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_file(archive_path);
    }

    #[test]
    fn env_archives_round_trip_metadata_and_root_contents() {
        let source_root = temp_path("source-root");
        let archive_path = temp_path("archives").join("demo.ocm-env.tar");
        let extract_dir = temp_path("extract");
        fs::create_dir_all(source_root.join(".openclaw/workspace")).unwrap();
        fs::write(
            source_root.join(".openclaw/workspace/notes.txt"),
            "hello archive",
        )
        .unwrap();

        let exported_at = time::OffsetDateTime::from_unix_timestamp(1_774_497_600).unwrap();
        let metadata = EnvArchiveMetadata {
            kind: "ocm-env-archive".to_string(),
            format_version: 1,
            exported_at,
            env: ArchivedEnvMeta {
                name: "demo".to_string(),
                source_root: Some(source_root.display().to_string()),
                gateway_port: Some(19789),
                gateway_port_auto_assigned: false,
                service_enabled: true,
                service_running: true,
                default_runtime: Some("stable".to_string()),
                default_launcher: Some("shell".to_string()),
                protected: true,
                created_at: exported_at,
                updated_at: exported_at,
                last_used_at: None,
            },
        };

        write_env_archive(&metadata, &source_root, &archive_path).unwrap();
        let extracted =
            extract_env_archive::<EnvArchiveMetadata>(&archive_path, &extract_dir).unwrap();

        assert_eq!(extracted.metadata, metadata);
        assert_eq!(
            fs::read_to_string(extracted.root_dir.join(".openclaw/workspace/notes.txt")).unwrap(),
            "hello archive"
        );

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(extract_dir);
        let _ = fs::remove_file(archive_path);
    }
}
