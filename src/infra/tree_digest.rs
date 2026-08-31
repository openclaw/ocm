use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeInventoryEntry {
    kind: &'static str,
    pub(crate) len: Option<u64>,
    mode: u32,
    link_target: Option<PathBuf>,
    sha256: Option<[u8; 32]>,
}

/// Returns a deterministic inventory of a tree without following symlinks.
pub(crate) fn inventory_tree(root: &Path) -> Result<BTreeMap<PathBuf, TreeInventoryEntry>, String> {
    let mut out = BTreeMap::new();
    inventory_path(root, root, &mut out)?;
    Ok(out)
}

fn inventory_path(
    root: &Path,
    path: &Path,
    out: &mut BTreeMap<PathBuf, TreeInventoryEntry>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let file_type = metadata.file_type();
    let (kind, link_target, sha256) = if file_type.is_symlink() {
        (
            "symlink",
            Some(fs::read_link(path).map_err(|error| error.to_string())?),
            None,
        )
    } else if file_type.is_dir() {
        ("directory", None, None)
    } else if file_type.is_file() {
        ("file", None, Some(hash_file(path)?))
    } else {
        ("special", None, None)
    };
    out.insert(
        relative,
        TreeInventoryEntry {
            kind,
            len: file_type.is_file().then_some(metadata.len()),
            mode: metadata_mode(&metadata),
            link_target,
            sha256,
        },
    );
    if file_type.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            inventory_path(root, &entry.path(), out)?;
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

/// Returns a SHA-256 digest covering paths, types, modes, file bytes, and symlink targets.
///
/// Entries are sorted, symlinks are not followed, and timestamps, ownership, and inode identity
/// are excluded. Paths and symlink targets must be UTF-8 so each package tree has one encoding.
pub(crate) fn tree_sha256(root: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"ocm-runtime-tree-v1\0");
    for (path, entry) in inventory_tree(root)? {
        if entry.kind == "special" {
            return Err(format!(
                "runtime tree contains unsupported entry type: {}",
                root.join(path).display()
            ));
        }
        hash_path(&path, &mut hasher)?;
        hash_bytes(entry.kind.as_bytes(), &mut hasher);
        hasher.update(entry.mode.to_be_bytes());
        hasher.update(entry.len.unwrap_or(u64::MAX).to_be_bytes());
        if let Some(target) = entry.link_target {
            hash_path(&target, &mut hasher)?;
        }
        if let Some(sha256) = entry.sha256 {
            hasher.update(sha256);
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn hash_path(path: &Path, hasher: &mut Sha256) -> Result<(), String> {
    let components = path.components().collect::<Vec<_>>();
    hasher.update((components.len() as u64).to_be_bytes());
    for component in components {
        let (kind, value) = match component {
            Component::Prefix(value) => (b'p', value.as_os_str()),
            Component::RootDir => (b'r', std::ffi::OsStr::new("")),
            Component::CurDir => (b'c', std::ffi::OsStr::new("")),
            Component::ParentDir => (b'u', std::ffi::OsStr::new("")),
            Component::Normal(value) => (b'n', value),
        };
        let value = value
            .to_str()
            .ok_or_else(|| format!("runtime tree path is not valid UTF-8: {}", path.display()))?;
        hasher.update([kind]);
        hash_bytes(value.as_bytes(), hasher);
    }
    Ok(())
}

fn hash_bytes(value: &[u8], hasher: &mut Sha256) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode()
}

#[cfg(not(unix))]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
}

#[cfg(test)]
mod tests {
    use super::tree_sha256;

    #[cfg(unix)]
    #[test]
    fn tree_digest_preserves_v1_encoding() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(
            tree_sha256(root.path()).unwrap(),
            "47d8bf62e61e92d5b7e76a8669b829db917ffdeaf8e1b6484ec4882c3d42d005"
        );
    }

    #[test]
    fn tree_digest_is_stable_and_covers_nested_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("dist/control-ui")).unwrap();
        std::fs::write(root.path().join("openclaw.mjs"), b"launcher").unwrap();
        std::fs::write(root.path().join("dist/control-ui/index.js"), b"first").unwrap();

        let first = tree_sha256(root.path()).unwrap();
        assert_eq!(tree_sha256(root.path()).unwrap(), first);

        std::fs::write(root.path().join("dist/control-ui/index.js"), b"second").unwrap();
        assert_ne!(tree_sha256(root.path()).unwrap(), first);
    }

    #[cfg(unix)]
    #[test]
    fn tree_digest_covers_modes_and_symlink_targets_without_following_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("target");
        std::fs::write(&file, b"same bytes").unwrap();
        symlink("target", root.path().join("link")).unwrap();
        let first = tree_sha256(root.path()).unwrap();

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mode_changed = tree_sha256(root.path()).unwrap();
        assert_ne!(mode_changed, first);
        std::fs::remove_file(root.path().join("link")).unwrap();
        symlink("missing", root.path().join("link")).unwrap();
        assert_ne!(tree_sha256(root.path()).unwrap(), mode_changed);
    }
}
