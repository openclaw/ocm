use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::Serialize;
use serde::de::DeserializeOwned;

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

pub(crate) struct ExclusiveFileLock {
    file: File,
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
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
    Ok(ExclusiveFileLock { file })
}

pub(crate) fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    ensure_dir(destination)?;

    let entries = fs::read_dir(source)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            Ok((entry.path(), destination.join(entry.file_name())))
        })
        .collect::<Result<Vec<_>, String>>()?;

    for (source_path, destination_path) in entries {
        copy_path(&source_path, &destination_path)?;
    }

    Ok(())
}

pub(crate) fn copy_path_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    copy_path(source, destination)
}

pub(crate) fn copy_path(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        return copy_symlink(source, destination);
    }
    if file_type.is_dir() {
        return copy_dir_recursive(source, destination);
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
fn replace_path(source: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(source, destination).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn replace_path(source: &Path, destination: &Path) -> Result<(), String> {
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
    use super::{copy_dir_recursive, write_file_replacing_path};

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
}
