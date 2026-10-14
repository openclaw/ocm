use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tar::{Archive, Builder, Header};
use time::OffsetDateTime;
use zip::ZipArchive;

use crate::env::{default_service_enabled, default_service_running};
#[cfg(not(target_os = "linux"))]
use crate::infra::sqlite_snapshot::create_sqlite_snapshot_no_follow;
use crate::infra::sqlite_snapshot::{create_sqlite_snapshot, file_is_sqlite_database};

pub const ENV_ARCHIVE_METADATA_PATH: &str = "meta/env.json";
pub const ENV_ARCHIVE_ROOT_DIR: &str = "root";
const MAX_ENV_ARCHIVE_WRITE_ATTEMPTS: usize = 2;
const SQLITE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

#[derive(Debug)]
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_state: bool,
    pub kind: String,
    pub format_version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub exported_at: OffsetDateTime,
    pub env: ArchivedEnvMeta,
}

fn is_false(value: &bool) -> bool {
    !value
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
    pub should_exclude_path: fn(&Path, EnvArchiveEntryKind) -> bool,
    pub included_path_roots: BTreeSet<PathBuf>,
    pub excluded_path_roots: BTreeSet<PathBuf>,
    pub snapshot_sqlite_files: bool,
    pub private_output: bool,
    pub relocate_internal_symlinks: bool,
    pub sqlite_content_roots: BTreeSet<PathBuf>,
}

impl Default for EnvArchiveOptions {
    fn default() -> Self {
        Self {
            should_skip_path: include_env_archive_path,
            should_exclude_path: exclude_no_env_archive_path,
            included_path_roots: BTreeSet::new(),
            excluded_path_roots: BTreeSet::new(),
            snapshot_sqlite_files: false,
            private_output: false,
            relocate_internal_symlinks: false,
            sqlite_content_roots: BTreeSet::new(),
        }
    }
}

impl EnvArchiveOptions {
    pub(crate) fn should_skip(&self, relative_path: &Path, kind: EnvArchiveEntryKind) -> bool {
        if (self.should_exclude_path)(relative_path, kind)
            || self
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

fn exclude_no_env_archive_path(_relative_path: &Path, _kind: EnvArchiveEntryKind) -> bool {
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
    mut options: EnvArchiveOptions,
) -> Result<(), String> {
    // Resolve aliases once. A retry must not retarget the selected environment.
    let resolved_root = if options.private_output
        || !options.sqlite_content_roots.is_empty()
        || options.relocate_internal_symlinks
    {
        Some(
            source_root
                .canonicalize()
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let source_root = resolved_root.as_deref().unwrap_or(source_root);
    let capture = if options.sqlite_content_roots.is_empty() && !options.relocate_internal_symlinks
    {
        None
    } else {
        Some(
            ArchiveSource::open_canonical(source_root)
                .map_err(EnvArchiveWriteError::into_message)?,
        )
    };
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let staged = if options.private_output {
        let mut builder = tempfile::Builder::new();
        builder.prefix(".ocm-export-");
        #[cfg(target_os = "macos")]
        let file = builder.make_in(
            parent,
            crate::infra::macos_security::create_private_file_new,
        );
        #[cfg(windows)]
        let file = builder.make_in(
            parent,
            crate::infra::windows_security::create_private_file_new,
        );
        #[cfg(not(any(windows, target_os = "macos")))]
        let file = builder.tempfile_in(parent);
        let file = file.map_err(|error| error.to_string())?;
        // An output requested inside the environment must not capture its own staging file.
        if let Ok(relative) = file
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?
            .strip_prefix(source_root)
        {
            options.excluded_path_roots.insert(relative.to_path_buf());
        }
        Some(file)
    } else {
        None
    };
    for attempt in 0..MAX_ENV_ARCHIVE_WRITE_ATTEMPTS {
        let file = if let Some(staged) = &staged {
            let mut file = staged
                .as_file()
                .try_clone()
                .map_err(|error| error.to_string())?;
            file.set_len(0).map_err(|error| error.to_string())?;
            file.seek(SeekFrom::Start(0))
                .map_err(|error| error.to_string())?;
            file
        } else {
            File::create(output_path).map_err(|error| error.to_string())?
        };
        match write_env_archive_attempt(
            metadata,
            source_root,
            file,
            options.clone(),
            capture.as_ref(),
        ) {
            Ok(()) => {
                if let Some(staged) = staged {
                    staged
                        .persist_noclobber(output_path)
                        .map_err(|error| error.to_string())?;
                }
                return Ok(());
            }
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
    file: File,
    options: EnvArchiveOptions,
    capture: Option<&ArchiveSource>,
) -> Result<(), EnvArchiveWriteError> {
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
    append_env_root(&mut builder, source_root, &options, capture)?;
    builder
        .finish()
        .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))
}

fn append_env_root(
    builder: &mut Builder<File>,
    source_root: &Path,
    options: &EnvArchiveOptions,
    capture: Option<&ArchiveSource>,
) -> Result<(), EnvArchiveWriteError> {
    let source_root = capture
        .map(|source| source.root.as_path())
        .unwrap_or(source_root);
    if let Some(source) = capture {
        append_directory_header(
            builder,
            Path::new(ENV_ARCHIVE_ROOT_DIR),
            &source.metadata()?,
        )?;
    } else {
        builder
            .append_dir(ENV_ARCHIVE_ROOT_DIR, source_root)
            .map_err(|error| {
                EnvArchiveWriteError::Fatal(format!(
                    "failed to archive {}: {error}",
                    source_root.display()
                ))
            })?;
    }
    let mut stack = if let Some(source) = capture {
        source
            .children(Path::new(""), &source.metadata()?)?
            .1
            .into_iter()
            .map(|name| source_root.join(name))
            .collect()
    } else {
        sorted_child_paths(source_root)
            .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?
    };
    let mut state_databases = BTreeSet::new();
    let mut state_sidecars = BTreeSet::new();
    if options.sqlite_content_roots.contains(Path::new("")) {
        classify_sqlite_children(
            &stack,
            source_root,
            capture.unwrap(),
            options,
            &mut state_databases,
            &mut state_sidecars,
        )?;
    }
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
        let discover_sqlite = options
            .sqlite_content_roots
            .iter()
            .any(|root| relative_path.starts_with(root));
        let mut captured = if discover_sqlite && metadata.is_file() {
            Some(capture.unwrap().file(relative_path, &metadata)?)
        } else {
            None
        };
        let state_database = if let Some(captured) = &mut captured {
            state_databases.contains(&path)
                || file_is_sqlite_database(&mut captured.file)
                    .map_err(EnvArchiveWriteError::Fatal)?
        } else {
            false
        };
        if state_database {
            state_sidecars.extend(sqlite_sidecar_paths(&path));
        } else if let Some(captured) = &mut captured {
            if state_sidecars.contains(&path)
                || is_orphan_sqlite_sidecar(&path, &mut captured.file)
                    .map_err(EnvArchiveWriteError::from_entry_io)?
            {
                continue;
            }
        }
        if !discover_sqlite
            && options.snapshot_sqlite_files
            && sqlite_database_path_for_sidecar(relative_path).is_some()
        {
            continue;
        }

        let archive_path = Path::new(ENV_ARCHIVE_ROOT_DIR).join(relative_path);
        if metadata.is_dir() {
            let mut children = if let Some(source) = capture {
                let (directory_metadata, names) = source.children(relative_path, &metadata)?;
                append_directory_header(builder, &archive_path, &directory_metadata)?;
                names.into_iter().map(|name| path.join(name)).collect()
            } else {
                builder.append_dir(&archive_path, &path).map_err(|error| {
                    EnvArchiveWriteError::from_entry_io(io_error_with_context(
                        error,
                        format!("failed to archive {}", path.display()),
                    ))
                })?;
                sorted_child_paths(&path).map_err(EnvArchiveWriteError::from_entry_io)?
            };
            if discover_sqlite {
                classify_sqlite_children(
                    &children,
                    source_root,
                    capture.unwrap(),
                    options,
                    &mut state_databases,
                    &mut state_sidecars,
                )?;
            }
            stack.append(&mut children);
            continue;
        }

        if state_database
            || (!discover_sqlite
                && options.snapshot_sqlite_files
                && is_sqlite_database_path(relative_path))
        {
            let source_metadata = captured
                .as_ref()
                .map(|captured| captured.file.metadata())
                .unwrap_or_else(|| fs::metadata(&path))
                .map_err(|error| {
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
            let snapshot = if let Some(source) = capture.filter(|_| discover_sqlite) {
                #[cfg(target_os = "macos")]
                let guard = super::archive_path_guard::SqlitePathGuard::new(
                    &source.root,
                    &source.metadata()?,
                    &source.root.join(relative_path),
                    &source_metadata,
                )
                .map_err(EnvArchiveWriteError::from_entry_io)?;
                #[cfg(target_os = "linux")]
                let snapshot = {
                    let parent =
                        source.directory(relative_path.parent().unwrap_or(Path::new("")), None)?;
                    super::sqlite_snapshot::create_sqlite_snapshot_in_directory(
                        &parent.file,
                        relative_path.file_name().unwrap(),
                    )
                };
                #[cfg(not(target_os = "linux"))]
                let snapshot = create_sqlite_snapshot_no_follow(&source.root.join(relative_path));
                #[cfg(target_os = "macos")]
                guard.check().map_err(EnvArchiveWriteError::from_entry_io)?;
                // A replaced primary invalidates this attempt, including its sidecar decisions.
                source.file(relative_path, &source_metadata)?;
                snapshot.map_err(EnvArchiveWriteError::Fatal)?
            } else {
                create_sqlite_snapshot(&path).map_err(EnvArchiveWriteError::Fatal)?
            };
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

        if let Some(mut captured) = captured {
            let captured_metadata = captured
                .file
                .metadata()
                .map_err(EnvArchiveWriteError::from_entry_io)?;
            append_bounded_file(
                builder,
                &archive_path,
                &mut captured.file,
                &captured_metadata,
            )?;
            continue;
        }

        if metadata.is_symlink() && (discover_sqlite || options.relocate_internal_symlinks) {
            let source = capture.unwrap();
            let captured_target = source.link(relative_path, &metadata)?;
            let owned = if options.relocate_internal_symlinks {
                owned_archive_link_target(
                    &source.root.join(relative_path),
                    &source.root,
                    &captured_target,
                )?
            } else {
                None
            };
            #[cfg(windows)]
            let directory_link = {
                use std::os::windows::fs::FileTypeExt;
                metadata.file_type().is_symlink_dir()
            };
            #[cfg(not(windows))]
            let directory_link = owned
                .as_ref()
                .is_some_and(|(_, relative)| source.directory(relative, None).is_ok());
            let target = owned.map(|(target, _)| target).unwrap_or(captured_target);
            if directory_link {
                // Established PAX metadata preserves directory links on Windows.
                builder
                    .append_pax_extensions([("LIBARCHIVE.symlinktype", &b"dir"[..])])
                    .map_err(EnvArchiveWriteError::from_entry_io)?;
            }
            let mut header = Header::new_gnu();
            header.set_metadata(&metadata);
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            builder
                .append_link(&mut header, &archive_path, &target)
                .map_err(EnvArchiveWriteError::from_entry_io)?;
            continue;
        }
        if discover_sqlite {
            return Err(EnvArchiveWriteError::EntryDisappeared(
                "archive entry changed type".to_string(),
            ));
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

fn append_directory_header(
    builder: &mut Builder<File>,
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), EnvArchiveWriteError> {
    let mut header = Header::new_gnu();
    header.set_metadata(metadata);
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, io::empty())
        .map_err(EnvArchiveWriteError::from_entry_io)
}

struct ArchiveSource {
    root: PathBuf,
    #[cfg(unix)]
    directory: File,
    #[cfg(windows)]
    _directories: Vec<File>,
}

struct CapturedArchiveFile {
    file: File,
    #[cfg(windows)]
    _directories: Vec<File>,
}

impl ArchiveSource {
    fn open_canonical(root: &Path) -> Result<Self, EnvArchiveWriteError> {
        if !root.is_absolute() {
            return Err(EnvArchiveWriteError::Fatal(
                "archive root must be absolute".to_string(),
            ));
        }
        let root = root.to_path_buf();
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::fd::{AsRawFd, FromRawFd};
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::OpenOptionsExt;
            let components: Vec<_> = root
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(name) => Some(name),
                    _ => None,
                })
                .collect();
            #[cfg(target_os = "linux")]
            let search = libc::O_PATH;
            #[cfg(target_vendor = "apple")]
            let search = libc::O_SEARCH;
            #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
            let search = libc::O_RDONLY;
            let mut directory = fs::OpenOptions::new()
                .read(true)
                .custom_flags(
                    libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | if components.is_empty() {
                            libc::O_RDONLY
                        } else {
                            search
                        },
                )
                .open("/")
                .map_err(EnvArchiveWriteError::from_entry_io)?;
            for (index, component) in components.iter().enumerate() {
                let name = CString::new(component.as_bytes())
                    .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
                let flags = libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | if index + 1 == components.len() {
                        libc::O_RDONLY
                    } else {
                        search
                    };
                let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
                if fd < 0 {
                    let error = io::Error::last_os_error();
                    return Err(
                        if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                            EnvArchiveWriteError::EntryDisappeared(
                                "archive root changed during capture".to_string(),
                            )
                        } else {
                            EnvArchiveWriteError::from_entry_io(error)
                        },
                    );
                }
                directory = unsafe { File::from_raw_fd(fd) };
            }
            Ok(Self { root, directory })
        }
        #[cfg(windows)]
        {
            let mut directories = Vec::new();
            let mut path = PathBuf::new();
            for component in root.components() {
                path.push(component.as_os_str());
                if matches!(
                    component,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                ) {
                    directories.push(open_archive_windows_entry(&path, true)?);
                }
            }
            Ok(Self {
                root,
                _directories: directories,
            })
        }
        #[cfg(not(any(unix, windows)))]
        Ok(Self { root })
    }

    fn directory(
        &self,
        relative: &Path,
        expected: Option<&fs::Metadata>,
    ) -> Result<CapturedArchiveFile, EnvArchiveWriteError> {
        let components = relative
            .components()
            .map(|component| match component {
                std::path::Component::Normal(name) => Ok(name),
                _ => Err(EnvArchiveWriteError::Fatal(
                    "invalid relative archive directory".to_string(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        #[cfg(unix)]
        let file = self.open_unix(&components, true)?;
        #[cfg(windows)]
        let (file, directories) = {
            let mut path = self.root.clone();
            let mut directories = Vec::new();
            for component in components {
                path.push(component);
                directories.push(open_archive_windows_entry(&path, true)?);
            }
            let file = directories
                .last()
                .unwrap_or_else(|| self._directories.last().unwrap())
                .try_clone()
                .map_err(EnvArchiveWriteError::from_entry_io)?;
            (file, directories)
        };
        #[cfg(not(any(unix, windows)))]
        let file =
            File::open(self.root.join(relative)).map_err(EnvArchiveWriteError::from_entry_io)?;
        let actual = file
            .metadata()
            .map_err(EnvArchiveWriteError::from_entry_io)?;
        if !actual.is_dir()
            || expected.is_some_and(|expected| !archive_identity_matches(expected, &actual))
        {
            return Err(EnvArchiveWriteError::EntryDisappeared(
                "archive directory was replaced".to_string(),
            ));
        }
        Ok(CapturedArchiveFile {
            file,
            #[cfg(windows)]
            _directories: directories,
        })
    }

    fn children(
        &self,
        relative: &Path,
        expected: &fs::Metadata,
    ) -> Result<(fs::Metadata, Vec<std::ffi::OsString>), EnvArchiveWriteError> {
        let directory = self.directory(relative, Some(expected))?;
        let metadata = directory
            .file
            .metadata()
            .map_err(EnvArchiveWriteError::from_entry_io)?;
        #[cfg(unix)]
        let mut names = archive_directory_names(&directory.file)
            .map_err(EnvArchiveWriteError::from_entry_io)?;
        #[cfg(not(unix))]
        let mut names = fs::read_dir(self.root.join(relative))
            .map_err(EnvArchiveWriteError::from_entry_io)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()
            .map_err(EnvArchiveWriteError::from_entry_io)?;
        names.sort();
        names.reverse();
        Ok((metadata, names))
    }

    fn metadata(&self) -> Result<fs::Metadata, EnvArchiveWriteError> {
        #[cfg(unix)]
        let metadata = self.directory.metadata();
        #[cfg(windows)]
        let metadata = self._directories.last().unwrap().metadata();
        #[cfg(not(any(unix, windows)))]
        let metadata = fs::metadata(&self.root);
        metadata.map_err(EnvArchiveWriteError::from_entry_io)
    }

    #[cfg(unix)]
    fn open_unix(
        &self,
        components: &[&std::ffi::OsStr],
        directory: bool,
    ) -> Result<File, EnvArchiveWriteError> {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let mut file = self
            .directory
            .try_clone()
            .map_err(EnvArchiveWriteError::from_entry_io)?;
        for (index, name) in components.iter().enumerate() {
            let name = CString::new(name.as_bytes())
                .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if directory || index + 1 < components.len() {
                    libc::O_DIRECTORY
                } else {
                    0
                };
            let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                let error = io::Error::last_os_error();
                return Err(
                    if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                        EnvArchiveWriteError::EntryDisappeared(
                            "archive source was replaced".to_string(),
                        )
                    } else {
                        EnvArchiveWriteError::from_entry_io(error)
                    },
                );
            }
            file = unsafe { File::from_raw_fd(fd) };
        }
        Ok(file)
    }

    fn link(
        &self,
        relative: &Path,
        expected: &fs::Metadata,
    ) -> Result<PathBuf, EnvArchiveWriteError> {
        let components = relative
            .components()
            .map(|component| match component {
                std::path::Component::Normal(name) => Ok(name),
                _ => Err(EnvArchiveWriteError::Fatal(
                    "invalid relative archive path".to_string(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some((leaf, parents)) = components.split_last() else {
            return Err(EnvArchiveWriteError::Fatal(
                "empty archive link path".to_string(),
            ));
        };
        #[cfg(unix)]
        {
            use std::ffi::{CString, OsString};
            use std::os::fd::AsRawFd;
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            use std::os::unix::fs::MetadataExt;
            let directory = self.open_unix(parents, true)?;
            let name = CString::new(leaf.as_bytes())
                .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
            let inspect = || -> Result<(), EnvArchiveWriteError> {
                let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
                if unsafe {
                    libc::fstatat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        metadata.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } != 0
                {
                    return Err(EnvArchiveWriteError::from_entry_io(
                        io::Error::last_os_error(),
                    ));
                }
                let metadata = unsafe { metadata.assume_init() };
                if metadata.st_mode & libc::S_IFMT != libc::S_IFLNK
                    || metadata.st_dev as u64 != expected.dev()
                    || metadata.st_ino as u64 != expected.ino()
                {
                    return Err(EnvArchiveWriteError::EntryDisappeared(
                        "archive link was replaced".to_string(),
                    ));
                }
                Ok(())
            };
            inspect()?;
            let mut buffer = vec![0_u8; 256];
            loop {
                let count = unsafe {
                    libc::readlinkat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                    )
                };
                if count < 0 {
                    return Err(EnvArchiveWriteError::EntryDisappeared(
                        "archive link changed during capture".to_string(),
                    ));
                }
                if (count as usize) < buffer.len() {
                    buffer.truncate(count as usize);
                    inspect()?;
                    return Ok(PathBuf::from(OsString::from_vec(buffer)));
                }
                if buffer.len() >= 1024 * 1024 {
                    return Err(EnvArchiveWriteError::Fatal(
                        "archive link target is too long".to_string(),
                    ));
                }
                buffer.resize(buffer.len() * 2, 0);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
            };
            let mut path = self.root.clone();
            let mut directories = Vec::new();
            for parent in parents {
                path.push(parent);
                directories.push(open_archive_windows_entry(&path, true)?);
            }
            path.push(leaf);
            let link = fs::OpenOptions::new()
                .access_mode(0)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
                .map_err(EnvArchiveWriteError::from_entry_io)?;
            let metadata = link
                .metadata()
                .map_err(EnvArchiveWriteError::from_entry_io)?;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
                || metadata.creation_time() != expected.creation_time()
            {
                return Err(EnvArchiveWriteError::EntryDisappeared(
                    "archive link was replaced".to_string(),
                ));
            }
            fs::read_link(path).map_err(EnvArchiveWriteError::from_entry_io)
        }
        #[cfg(not(any(unix, windows)))]
        fs::read_link(self.root.join(relative)).map_err(EnvArchiveWriteError::from_entry_io)
    }

    fn file(
        &self,
        relative: &Path,
        expected: &fs::Metadata,
    ) -> Result<CapturedArchiveFile, EnvArchiveWriteError> {
        let components = relative
            .components()
            .map(|component| match component {
                std::path::Component::Normal(name) => Ok(name),
                _ => Err(EnvArchiveWriteError::Fatal(
                    "invalid relative archive path".to_string(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if components.is_empty() {
            return Err(EnvArchiveWriteError::Fatal(
                "empty archive file path".to_string(),
            ));
        }
        #[cfg(windows)]
        let (leaf, parents) = components.split_last().unwrap();
        #[cfg(unix)]
        let file = self.open_unix(&components, false)?;
        #[cfg(windows)]
        let (file, directories) = {
            let mut path = self.root.clone();
            let mut directories = Vec::new();
            for parent in parents {
                path.push(parent);
                directories.push(open_archive_windows_entry(&path, true)?);
            }
            path.push(leaf);
            (open_archive_windows_entry(&path, false)?, directories)
        };
        #[cfg(not(any(unix, windows)))]
        let file =
            File::open(self.root.join(relative)).map_err(EnvArchiveWriteError::from_entry_io)?;
        let actual = file
            .metadata()
            .map_err(EnvArchiveWriteError::from_entry_io)?;
        let same = archive_identity_matches(expected, &actual);
        if !actual.is_file() || !same {
            return Err(EnvArchiveWriteError::EntryDisappeared(
                "archive source was replaced".to_string(),
            ));
        }
        Ok(CapturedArchiveFile {
            file,
            #[cfg(windows)]
            _directories: directories,
        })
    }
}

fn archive_identity_matches(expected: &fs::Metadata, actual: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        actual.dev() == expected.dev() && actual.ino() == expected.ino()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        actual.creation_time() == expected.creation_time()
    }
    #[cfg(not(any(unix, windows)))]
    {
        actual.created().ok() == expected.created().ok()
    }
}

#[cfg(unix)]
fn archive_directory_names(directory: &File) -> io::Result<Vec<std::ffi::OsString>> {
    use std::ffi::{CStr, OsString};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStringExt;
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0) };
        }
    }
    // A fresh open description gives each traversal its own directory cursor.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        drop(unsafe { File::from_raw_fd(fd) });
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
        let errno = unsafe { libc::__error() };
        #[cfg(any(target_os = "linux", target_os = "dragonfly"))]
        let errno = unsafe { libc::__errno_location() };
        #[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
        let errno = unsafe { libc::__errno() };
        #[cfg(any(target_os = "solaris", target_os = "illumos"))]
        let errno = unsafe { libc::___errno() };
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "freebsd",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "android",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "solaris",
            target_os = "illumos"
        )))]
        let errno: *mut libc::c_int = std::ptr::null_mut();
        if errno.is_null() {
            return Err(io::Error::other(
                "directory enumeration is unavailable on this platform",
            ));
        }
        unsafe { *errno = 0 };
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = unsafe { *errno };
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
            return Ok(names);
        }
        // readdir owns this NUL-terminated name until the next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
}

#[cfg(windows)]
fn open_archive_windows_entry(path: &Path, directory: bool) -> Result<File, EnvArchiveWriteError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = fs::OpenOptions::new();
    if directory {
        options.access_mode(0);
    } else {
        options.read(true);
    }
    // Denying delete-sharing pins all ancestors and the selected file against replacement.
    let file = options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(EnvArchiveWriteError::from_entry_io)?;
    let metadata = file
        .metadata()
        .map_err(EnvArchiveWriteError::from_entry_io)?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(EnvArchiveWriteError::EntryDisappeared(
            "archive source was replaced".to_string(),
        ));
    }
    Ok(file)
}

fn append_bounded_file(
    builder: &mut Builder<File>,
    archive_path: &Path,
    file: &mut File,
    metadata: &fs::Metadata,
) -> Result<(), EnvArchiveWriteError> {
    let mut header = Header::new_gnu();
    header.set_metadata(metadata);
    header.set_cksum();
    // Appends after metadata capture belong to a later export. Reading to EOF
    // could write beyond the tar header and misalign subsequent entries.
    let mut contents = file.take(metadata.len());
    builder
        .append_data(&mut header, archive_path, &mut contents)
        .map_err(EnvArchiveWriteError::from_entry_io)?;
    if contents.limit() != 0 {
        return Err(EnvArchiveWriteError::EntryDisappeared(format!(
            "file shrank while archiving {}",
            archive_path.display()
        )));
    }
    Ok(())
}

fn owned_archive_link_target(
    path: &Path,
    source_root: &Path,
    captured_target: &Path,
) -> Result<Option<(PathBuf, PathBuf)>, EnvArchiveWriteError> {
    let target_path = if captured_target.is_absolute() {
        captured_target.to_path_buf()
    } else {
        path.parent().unwrap().join(captured_target)
    };
    let target = match fs::canonicalize(target_path) {
        Ok(target) => target,
        Err(_) => return Ok(None),
    };
    let root = fs::canonicalize(source_root).map_err(EnvArchiveWriteError::from_entry_io)?;
    let Ok(target) = target.strip_prefix(&root) else {
        return Ok(None);
    };
    let parent = path
        .parent()
        .unwrap()
        .strip_prefix(source_root)
        .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
    let common = parent
        .components()
        .zip(target.components())
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in parent.components().skip(common) {
        relative.push("..");
    }
    for part in target.components().skip(common) {
        relative.push(part);
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Ok(Some((relative, target.to_path_buf())))
}

fn classify_sqlite_children(
    children: &[PathBuf],
    source_root: &Path,
    capture: &ArchiveSource,
    options: &EnvArchiveOptions,
    state_databases: &mut BTreeSet<PathBuf>,
    state_sidecars: &mut BTreeSet<PathBuf>,
) -> Result<(), EnvArchiveWriteError> {
    // Classify sibling primaries before writing any of their sidecars.
    // Retain this identity even if a live primary later disappears.
    for child in children {
        let child_meta =
            fs::symlink_metadata(child).map_err(EnvArchiveWriteError::from_entry_io)?;
        let child_relative = child
            .strip_prefix(source_root)
            .map_err(|error| EnvArchiveWriteError::Fatal(error.to_string()))?;
        if options.should_skip(child_relative, env_archive_entry_kind(&child_meta)) {
            continue;
        }
        if child_meta.is_file() {
            let mut captured = capture.file(child_relative, &child_meta)?;
            if file_is_sqlite_database(&mut captured.file).map_err(EnvArchiveWriteError::Fatal)? {
                state_databases.insert(child.clone());
                state_sidecars.extend(sqlite_sidecar_paths(child));
            }
        }
    }
    Ok(())
}

fn sqlite_sidecar_paths(path: &Path) -> [PathBuf; 3] {
    SQLITE_SIDECAR_SUFFIXES.map(|suffix| {
        let mut path = path.as_os_str().to_os_string();
        path.push(suffix);
        PathBuf::from(path)
    })
}

fn sqlite_sidecar_suffix(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.as_encoded_bytes();
    SQLITE_SIDECAR_SUFFIXES
        .into_iter()
        .find(|suffix| name.ends_with(suffix.as_bytes()))
}

fn is_orphan_sqlite_sidecar(path: &Path, file: &mut File) -> io::Result<bool> {
    let Some(suffix) = sqlite_sidecar_suffix(path) else {
        return Ok(false);
    };
    let mut header = Vec::new();
    (&mut *file).take(96).read_to_end(&mut header)?;
    file.seek(SeekFrom::Start(0))?;
    // SQLite's WAL/journal magic and duplicated wal-index header distinguish
    // orphaned process files from durable payloads with sidecar-like names.
    Ok(match suffix {
        "-wal" => {
            header.len() >= 8
                && matches!(
                    u32::from_be_bytes(header[0..4].try_into().unwrap()),
                    0x377f0682 | 0x377f0683
                )
                && u32::from_be_bytes(header[4..8].try_into().unwrap()) == 3_007_000
        }
        "-journal" => header.starts_with(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]),
        "-shm" => {
            header.len() == 96
                && header[..48] == header[48..]
                && header[12] == 1
                && (u32::from_le_bytes(header[0..4].try_into().unwrap()) == 3_007_000
                    || u32::from_be_bytes(header[0..4].try_into().unwrap()) == 3_007_000)
        }
        _ => false,
    })
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

pub(crate) fn restore_env_archive_directory_permissions(
    archive_path: &Path,
    staging_dir: &Path,
) -> Result<(), String> {
    let file = File::open(archive_path).map_err(|error| error.to_string())?;
    let mut archive = Archive::new(file);
    #[cfg(windows)]
    let mut directory_links = BTreeSet::new();
    // Seek over payloads: only directory headers need a second pass. Use tar's
    // containment checks, and never enable special permission bits on files.
    for entry in archive
        .entries_with_seek()
        .map_err(|error| error.to_string())?
    {
        let mut entry = entry.map_err(|error| error.to_string())?;
        #[cfg(windows)]
        {
            let path = entry
                .path()
                .map_err(|error| error.to_string())?
                .into_owned();
            directory_links.remove(&path);
            if path.starts_with(ENV_ARCHIVE_ROOT_DIR) && entry.header().entry_type().is_symlink() {
                if let Some(attributes) =
                    entry.pax_extensions().map_err(|error| error.to_string())?
                {
                    for attribute in attributes {
                        let attribute = attribute.map_err(|error| error.to_string())?;
                        if attribute.key_bytes() == b"LIBARCHIVE.symlinktype"
                            && attribute.value_bytes() == b"dir"
                        {
                            directory_links.insert(path.clone());
                        }
                    }
                }
            }
        }

        if entry.header().entry_type().is_dir()
            && entry
                .path()
                .map_err(|error| error.to_string())?
                .starts_with(ENV_ARCHIVE_ROOT_DIR)
        {
            entry.set_preserve_permissions(true);
            if !entry
                .unpack_in(staging_dir)
                .map_err(|error| error.to_string())?
            {
                return Err("archive directory escaped staging".to_string());
            }
        }
    }
    #[cfg(windows)]
    for path in directory_links {
        use std::os::windows::fs::FileTypeExt;
        let relative = path
            .strip_prefix(ENV_ARCHIVE_ROOT_DIR)
            .map_err(|_| "archive link is outside root/")?;
        let components = relative
            .components()
            .map(|component| match component {
                std::path::Component::Normal(name) => Ok(name),
                _ => Err("invalid archived directory link path"),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some((leaf, parents)) = components.split_last() else {
            return Err("empty archived directory link path".to_string());
        };
        let mut path = staging_dir.join(ENV_ARCHIVE_ROOT_DIR);
        for parent in parents {
            path.push(parent);
            let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if !metadata.is_dir() || metadata.is_symlink() {
                return Err("archived directory link has a linked parent".to_string());
            }
        }
        path.push(leaf);
        let kind = fs::symlink_metadata(&path)
            .map_err(|error| error.to_string())?
            .file_type();
        if kind.is_symlink_dir() {
            continue;
        }
        if !kind.is_symlink() {
            return Err("archived directory link changed type".to_string());
        }
        let target = fs::read_link(&path).map_err(|error| error.to_string())?;
        fs::remove_file(&path).map_err(|error| error.to_string())?;
        std::os::windows::fs::symlink_dir(target, &path).map_err(|error| error.to_string())?;
    }
    Ok(())
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
    #[cfg(unix)]
    use super::{SQLITE_SIDECAR_SUFFIXES, sqlite_sidecar_paths, sqlite_sidecar_suffix};
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

    #[cfg(unix)]
    #[test]
    fn sqlite_sidecar_names_do_not_require_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let database = PathBuf::from(std::ffi::OsString::from_vec(b"history-\xff.db".to_vec()));
        for (sidecar, suffix) in sqlite_sidecar_paths(&database)
            .iter()
            .zip(SQLITE_SIDECAR_SUFFIXES)
        {
            assert_eq!(sqlite_sidecar_suffix(sidecar), Some(suffix));
            assert!(sidecar.file_name().unwrap().to_str().is_none());
        }
    }

    #[cfg(unix)]
    static DISAPPEARING_PRIMARY: Mutex<Option<(PathBuf, usize)>> = Mutex::new(None);

    #[cfg(unix)]
    fn remove_primary_after_capture(relative: &Path, _kind: EnvArchiveEntryKind) -> bool {
        if relative == Path::new("state/live.db-wal") {
            let mut configured = DISAPPEARING_PRIMARY.lock().unwrap();
            let (path, visits) = configured.as_mut().unwrap();
            *visits += 1;
            if *visits == 2 {
                fs::remove_file(path).unwrap();
            }
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn env_archive_omits_sidecars_after_the_captured_primary_disappears() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(source.join("state")).unwrap();
        let primary = source.join("state/live.db");
        let writer = Connection::open(&primary).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records (value TEXT); INSERT INTO records VALUES ('committed');").unwrap();
        *DISAPPEARING_PRIMARY.lock().unwrap() = Some((primary.clone(), 0));
        let output = root.path().join("archive.tar");
        write_env_archive_with_options(
            &serde_json::json!({}),
            &source,
            &output,
            EnvArchiveOptions {
                should_exclude_path: remove_primary_after_capture,
                sqlite_content_roots: BTreeSet::from([PathBuf::from("state")]),
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap();
        let visits = DISAPPEARING_PRIMARY.lock().unwrap().take().unwrap().1;
        assert_eq!(visits, 2);
        assert!(!primary.exists());
        assert!(source.join("state/live.db-wal").exists());
        let extracted =
            extract_env_archive::<serde_json::Value>(&output, &root.path().join("extracted"))
                .unwrap();
        assert!(!extracted.root_dir.join("state/live.db-wal").exists());
        assert!(!extracted.root_dir.join("state/live.db-shm").exists());
        let db = Connection::open(extracted.root_dir.join("state/live.db")).unwrap();
        let value: String = db
            .query_row("SELECT value FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "committed");
    }

    #[cfg(unix)]
    #[test]
    fn root_acquisition_rejects_a_replaced_ancestor() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("parent");
        let outside = fixture.path().join("outside");
        fs::create_dir_all(parent.join("home")).unwrap();
        fs::create_dir_all(outside.join("home")).unwrap();
        let selected = parent.join("home").canonicalize().unwrap();
        fs::rename(&parent, fixture.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(outside, &parent).unwrap();
        assert!(super::ArchiveSource::open_canonical(&selected).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn root_acquisition_only_needs_search_access_to_ancestors() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("parent");
        let home = parent.join("home");
        fs::create_dir_all(&home).unwrap();
        let writer = Connection::open(home.join("data")).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE records(value TEXT); INSERT INTO records VALUES ('owned');",
            )
            .unwrap();
        drop(writer);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o111)).unwrap();
        let result = write_env_archive_with_options(
            &serde_json::json!({}),
            &home,
            &fixture.path().join("archive.tar"),
            EnvArchiveOptions {
                sqlite_content_roots: BTreeSet::from([PathBuf::new()]),
                private_output: true,
                ..EnvArchiveOptions::default()
            },
        );
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    static SQLITE_ROOT_REPLACEMENT: Mutex<Option<(PathBuf, PathBuf, usize)>> = Mutex::new(None);

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn replace_sqlite_root(relative: &Path, _kind: EnvArchiveEntryKind) -> bool {
        if relative == Path::new("history.db") {
            let mut fixture = SQLITE_ROOT_REPLACEMENT.lock().unwrap();
            let (root, other, visits) = fixture.as_mut().unwrap();
            *visits += 1;
            if *visits == 2 {
                fs::rename(&*root, root.with_file_name("saved")).unwrap();
                fs::rename(other, root).unwrap();
            }
        }
        false
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn archive_sqlite_does_not_capture_a_replacement_root() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("home");
        let other = fixture.path().join("other");
        for (path, value) in [(&root, "owned"), (&other, "outside")] {
            fs::create_dir(path).unwrap();
            let db = Connection::open(path.join("history.db")).unwrap();
            db.execute_batch("CREATE TABLE records(value TEXT)")
                .unwrap();
            db.execute("INSERT INTO records VALUES (?1)", [value])
                .unwrap();
        }
        *SQLITE_ROOT_REPLACEMENT.lock().unwrap() = Some((root.clone(), other, 0));
        let archive = fixture.path().join("archive.tar");
        let result = write_env_archive_with_options(
            &serde_json::json!({}),
            &root,
            &archive,
            EnvArchiveOptions {
                sqlite_content_roots: BTreeSet::from([PathBuf::new()]),
                should_exclude_path: replace_sqlite_root,
                private_output: true,
                ..EnvArchiveOptions::default()
            },
        );
        assert!(SQLITE_ROOT_REPLACEMENT.lock().unwrap().take().unwrap().2 >= 2);
        if result.is_ok() {
            let restored = extract_env_archive::<serde_json::Value>(
                &archive,
                &fixture.path().join("restored"),
            )
            .unwrap();
            let db = Connection::open(restored.root_dir.join("history.db")).unwrap();
            let value: String = db
                .query_row("SELECT value FROM records", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, "owned");
        } else {
            assert!(!archive.exists());
        }
    }

    #[cfg(unix)]
    static DIRECTORY_REPLACEMENT: Mutex<Option<(PathBuf, PathBuf, usize)>> = Mutex::new(None);

    #[cfg(unix)]
    fn replace_observed_directory(path: &Path, kind: EnvArchiveEntryKind) -> bool {
        if path == Path::new("history") && kind == EnvArchiveEntryKind::Directory {
            let mut fixture = DIRECTORY_REPLACEMENT.lock().unwrap();
            let (source, outside, visits) = fixture.as_mut().unwrap();
            *visits += 1;
            if *visits == 2 {
                fs::rename(source.join("history"), source.join("saved")).unwrap();
                std::os::unix::fs::symlink(outside, source.join("history")).unwrap();
            }
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn archive_directory_enumeration_does_not_follow_a_replacement() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("home");
        let outside = fixture.path().join("outside");
        fs::create_dir_all(source.join("history/owned")).unwrap();
        fs::create_dir_all(outside.join("external-directory/private-name")).unwrap();
        *DIRECTORY_REPLACEMENT.lock().unwrap() = Some((source.clone(), outside, 0));
        let output = fixture.path().join("archive.tar");
        write_env_archive_with_options(
            &serde_json::json!({}),
            &source,
            &output,
            EnvArchiveOptions {
                sqlite_content_roots: BTreeSet::from([PathBuf::new()]),
                should_exclude_path: replace_observed_directory,
                relocate_internal_symlinks: true,
                private_output: true,
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap();
        assert_eq!(DIRECTORY_REPLACEMENT.lock().unwrap().take().unwrap().2, 2);
        let mut archive = tar::Archive::new(fs::File::open(output).unwrap());
        for entry in archive.entries().unwrap() {
            assert!(
                !entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .contains("external-directory")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn owned_directory_links_include_portable_type_metadata() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("home");
        fs::create_dir_all(source.join("data")).unwrap();
        std::os::unix::fs::symlink("data", source.join("history")).unwrap();
        let output = root.path().join("archive.tar");
        write_env_archive_with_options(
            &serde_json::json!({}),
            &source,
            &output,
            EnvArchiveOptions {
                sqlite_content_roots: BTreeSet::from([PathBuf::new()]),
                relocate_internal_symlinks: true,
                private_output: true,
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap();
        let mut archive = tar::Archive::new(fs::File::open(output).unwrap());
        let mut found = false;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap() == Path::new("root/history") {
                assert!(entry.pax_extensions().unwrap().unwrap().any(|attribute| {
                    let attribute = attribute.unwrap();
                    attribute.key_bytes() == b"LIBARCHIVE.symlinktype"
                        && attribute.value_bytes() == b"dir"
                }));
                found = true;
            }
        }
        assert!(found);
    }

    #[cfg(unix)]
    static LINK_REPLACEMENT: Mutex<Option<(PathBuf, PathBuf, usize)>> = Mutex::new(None);

    #[cfg(unix)]
    fn replace_observed_link_parent(path: &Path, kind: EnvArchiveEntryKind) -> bool {
        if path == Path::new("data/link") && kind == EnvArchiveEntryKind::Symlink {
            let mut fixture = LINK_REPLACEMENT.lock().unwrap();
            let (source, outside, visits) = fixture.as_mut().unwrap();
            *visits += 1;
            if *visits == 2 {
                fs::rename(source.join("data"), source.join("saved")).unwrap();
                std::os::unix::fs::symlink(outside, source.join("data")).unwrap();
            }
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn archive_retries_a_replaced_link_parent_without_capturing_external_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("home");
        let outside = fixture.path().join("outside");
        fs::create_dir_all(source.join("data")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(source.join("owned"), b"owned").unwrap();
        fs::write(outside.join("link"), b"external").unwrap();
        std::os::unix::fs::symlink("../owned", source.join("data/link")).unwrap();
        *LINK_REPLACEMENT.lock().unwrap() = Some((source.clone(), outside.clone(), 0));
        let output = fixture.path().join("archive.tar");
        write_env_archive_with_options(
            &serde_json::json!({}),
            &source,
            &output,
            EnvArchiveOptions {
                sqlite_content_roots: BTreeSet::from([PathBuf::new()]),
                should_exclude_path: replace_observed_link_parent,
                relocate_internal_symlinks: true,
                private_output: true,
                ..EnvArchiveOptions::default()
            },
        )
        .unwrap();
        assert_eq!(LINK_REPLACEMENT.lock().unwrap().take().unwrap().2, 2);
        let mut archive = tar::Archive::new(fs::File::open(output).unwrap());
        let mut found = false;
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path().unwrap();
            if path == Path::new("root/data/link") {
                assert!(
                    entry.header().entry_type().is_symlink(),
                    "replacement exported external file contents"
                );
            }
            if path == Path::new("root/data") {
                if entry.header().entry_type().is_symlink() {
                    assert_eq!(entry.link_name().unwrap().unwrap(), outside);
                }
                found = true;
            }
        }
        assert!(found);
    }

    #[cfg(unix)]
    #[test]
    fn captured_file_rejects_leaf_and_parent_symlink_replacements() {
        use std::io::Read;
        for parent in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let outside = root.path().join("outside");
            fs::create_dir_all(home.join("data")).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(home.join("data/config"), b"owned").unwrap();
            fs::write(outside.join("config"), b"external").unwrap();
            let source =
                super::ArchiveSource::open_canonical(&home.canonicalize().unwrap()).unwrap();
            let observed = fs::symlink_metadata(home.join("data/config")).unwrap();
            let mut captured = source.file(Path::new("data/config"), &observed).unwrap();
            if parent {
                fs::rename(home.join("data"), home.join("saved")).unwrap();
                std::os::unix::fs::symlink(&outside, home.join("data")).unwrap();
            } else {
                fs::rename(home.join("data/config"), home.join("data/saved")).unwrap();
                std::os::unix::fs::symlink(outside.join("config"), home.join("data/config"))
                    .unwrap();
            }
            let mut bytes = Vec::new();
            captured.file.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"owned");
            assert!(matches!(
                source.file(Path::new("data/config"), &observed),
                Err(super::EnvArchiveWriteError::EntryDisappeared(_))
            ));
        }
    }

    #[cfg(windows)]
    #[test]
    fn private_config_archive_capture_pins_its_file_and_parents() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(home.join("data")).unwrap();
        let path = home.join("data/config");
        fs::write(&path, b"owned").unwrap();
        let source = super::ArchiveSource::open_canonical(&home.canonicalize().unwrap()).unwrap();
        let observed = fs::symlink_metadata(&path).unwrap();
        let captured = source.file(Path::new("data/config"), &observed).unwrap();
        assert!(fs::rename(&path, home.join("renamed")).is_err());
        assert!(fs::rename(home.join("data"), home.join("renamed-dir")).is_err());
        drop(captured);
        drop(source);
        fs::rename(&path, home.join("renamed")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn private_config_archive_link_capture_reads_the_reparse_entry() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let outside = root.path().join("outside");
        fs::create_dir_all(home.join("data")).unwrap();
        fs::create_dir(&outside).unwrap();
        let link = home.join("data").join("link");
        let created = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        let expected = fs::symlink_metadata(&link).unwrap();
        assert_eq!(
            super::env_archive_entry_kind(&expected),
            EnvArchiveEntryKind::Symlink
        );
        let source = super::ArchiveSource::open_canonical(&home.canonicalize().unwrap()).unwrap();
        assert_eq!(
            source.link(Path::new("data/link"), &expected).unwrap(),
            fs::read_link(&link).unwrap()
        );
    }

    #[test]
    fn private_archive_excludes_staging_and_preserves_existing_output() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("data"), b"fixture").unwrap();
        let output = root.path().join("archive.tar");
        let options = EnvArchiveOptions {
            private_output: true,
            ..EnvArchiveOptions::default()
        };
        write_env_archive_with_options(
            &serde_json::json!({}),
            root.path(),
            &output,
            options.clone(),
        )
        .unwrap();
        let original = fs::read(&output).unwrap();
        let mut archive = tar::Archive::new(fs::File::open(&output).unwrap());
        let paths: Vec<_> = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().into_owned())
            .collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("meta/env.json"),
                PathBuf::from("root"),
                PathBuf::from("root/data")
            ]
        );
        assert!(
            write_env_archive_with_options(&serde_json::json!({}), root.path(), &output, options)
                .is_err()
        );
        assert_eq!(fs::read(output).unwrap(), original);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn bounded_archive_file_ignores_growth_and_detects_truncation() {
        use std::fs::File;
        use std::io::{Read, Write};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("log");
        fs::write(&source, vec![b'a'; 513]).unwrap();
        let mut file = File::open(&source).unwrap();
        let metadata = file.metadata().unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap()
            .write_all(&vec![b'b'; 1024])
            .unwrap();
        let archive_path = root.path().join("archive.tar");
        let mut builder = tar::Builder::new(File::create(&archive_path).unwrap());
        assert!(
            super::append_bounded_file(&mut builder, Path::new("log"), &mut file, &metadata)
                .is_ok()
        );
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_cksum();
        builder
            .append_data(&mut header, "after", &b"next"[..])
            .unwrap();
        builder.finish().unwrap();
        let mut archive = tar::Archive::new(File::open(archive_path).unwrap());
        let mut entries = archive.entries().unwrap();
        let mut captured = Vec::new();
        entries
            .next()
            .unwrap()
            .unwrap()
            .read_to_end(&mut captured)
            .unwrap();
        assert_eq!(captured, vec![b'a'; 513]);
        assert_eq!(
            entries.next().unwrap().unwrap().path().unwrap(),
            Path::new("after")
        );
        assert!(entries.next().is_none());

        let mut file = File::open(&source).unwrap();
        let metadata = file.metadata().unwrap();
        fs::write(&source, b"short").unwrap();
        let mut builder = tar::Builder::new(File::create(root.path().join("short.tar")).unwrap());
        assert!(matches!(
            super::append_bounded_file(&mut builder, Path::new("log"), &mut file, &metadata),
            Err(super::EnvArchiveWriteError::EntryDisappeared(_))
        ));
    }

    #[test]
    fn archive_options_include_only_the_selected_path_branch() {
        let options = EnvArchiveOptions {
            should_skip_path: skip_openclaw_paths,
            included_path_roots: BTreeSet::from([PathBuf::from(".openclaw/team/ops")]),
            excluded_path_roots: BTreeSet::new(),
            snapshot_sqlite_files: false,
            ..EnvArchiveOptions::default()
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
            ..EnvArchiveOptions::default()
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
            include_state: false,
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
