use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use gix_config::AsBStr;

use crate::openclaw_repo::{
    git_command, git_identity_paths, git_path_from_bytes, git_registration_paths,
    normalize_git_file_path,
};

use super::{is_integrity_protected_history_destination, with_owner_write_permission};
use crate::store::layout::{EnvPaths, derive_env_paths, display_path};
use crate::store::openclaw_workspaces::OpenClawWorkspaceInventory;

/// The caller supplies already isolated Git directories, not source-selecting pointers.
pub(in crate::store) struct CopiedGitConfigContext {
    pub checkout: PathBuf,
    pub private_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Rewrite only copied structural configuration. Mappings describe original
/// absolute values; relative values use Git's containing-file or Git-dir context.
/// `root` is the owning environment root, not its `.openclaw` state directory.
/// Returned leaf/link paths must survive the caller's subsequent copy cleanup.
pub(in crate::store) fn relocate_copied_git_config(
    root: &Path,
    mappings: &[(PathBuf, PathBuf)],
    contexts: &[CopiedGitConfigContext],
) -> Result<BTreeSet<PathBuf>, String> {
    if contexts.is_empty() {
        return Ok(BTreeSet::new());
    }
    let canonical_root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let state_root = derive_env_paths(&canonical_root).state_dir;
    let mut retained = BTreeSet::new();
    let mut visited = BTreeMap::new();
    for context in contexts {
        for directory in [&context.checkout, &context.private_dir, &context.common_dir] {
            owned(&canonical_root, directory)?;
        }
        let config = context.common_dir.join("config");
        if config.exists() {
            edit_config(
                &canonical_root,
                &state_root,
                &config,
                &context.private_dir,
                mappings,
                &mut visited,
                &mut retained,
            )?;
        }
        loop {
            // Native Git owns conditional selection, including global settings
            // used by hasconfig. Values are not requested or exposed here.
            let output = git_command()
                .arg("--git-dir")
                .arg(git_path(&context.private_dir))
                .arg("--work-tree")
                .arg(git_path(&context.checkout))
                .env("GIT_COMMON_DIR", git_path(&context.common_dir))
                .args([
                    "config",
                    "--includes",
                    "--null",
                    "--show-origin",
                    "--name-only",
                    "--list",
                ])
                .output()
                .map_err(|e| e.to_string())?;
            let mut progressed = false;
            let mut source_origin = false;
            let mut fields = output.stdout.split(|b| *b == 0);
            while let Some(origin) = fields.next() {
                if origin.is_empty() {
                    continue;
                }
                let _key = fields
                    .next()
                    .ok_or("invalid Git configuration origin output")?;
                let Some(path) = origin.strip_prefix(b"file:") else {
                    continue;
                };
                let path = PathBuf::from(git_path_from_bytes(path)?);
                // Intermediate includes may still spell a source path. Process
                // only destination origins, then let Git select again after edits.
                if git_path(&path).starts_with(git_path(root))
                    || git_path(&path).starts_with(git_path(&canonical_root))
                {
                    if edit_config(
                        &canonical_root,
                        &state_root,
                        &path,
                        &context.private_dir,
                        mappings,
                        &mut visited,
                        &mut retained,
                    )? {
                        // Child origins were selected before this edit. Re-query
                        // immediately: overlapping source/destination prefixes
                        // can otherwise make a stale child name an unrelated copy.
                        progressed = true;
                        break;
                    }
                } else if !fs::canonicalize(&path).is_ok_and(|p| p.starts_with(&canonical_root))
                    && project_value(&path, &canonical_root, &canonical_root, mappings).is_some()
                {
                    source_origin = true;
                }
            }
            if !progressed {
                if !output.status.success() {
                    return Err("cannot resolve copied Git configuration".into());
                }
                if source_origin {
                    return Err("copied Git configuration still selects a source-owned file".into());
                }
                break;
            }
        }
    }
    // Cleanup traverses the caller's spelling (which may use an admitted root alias).
    Ok(retained
        .into_iter()
        .map(|path| {
            root.join(
                path.strip_prefix(&canonical_root)
                    .expect("owned retained path"),
            )
        })
        .collect())
}

/// Validate after the caller's cleanup, without an inspection worktree override.
pub(in crate::store) fn validate_copied_git_config(
    root: &Path,
    contexts: &[CopiedGitConfigContext],
) -> Result<(), String> {
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    for context in contexts {
        let output = git_command()
            .arg("-C")
            .arg(git_path(&context.checkout))
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|e| e.to_string())?;
        let path = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        let path = path.strip_suffix(b"\r").unwrap_or(path);
        let actual = PathBuf::from(git_path_from_bytes(path)?);
        if !output.status.success() || owned(&root, &actual).is_err() {
            // A bare repository has no worktree to validate. Its administrative
            // directories and selected config already passed ownership checks.
            let bare = git_command()
                .arg("-C")
                .arg(git_path(&context.checkout))
                .args(["rev-parse", "--is-bare-repository"])
                .output()
                .map_err(|e| e.to_string())?;
            if bare.status.success() && bare.stdout.trim_ascii() == b"true" {
                continue;
            }
            return Err(
                "copied Git configuration does not select a destination-owned checkout".into(),
            );
        }
    }
    Ok(())
}

pub(in crate::store) fn copied_workspace_git_contexts(
    source: &EnvPaths,
    target: &EnvPaths,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<Vec<CopiedGitConfigContext>, String> {
    let canonical_source = fs::canonicalize(&source.root).map_err(|e| e.to_string())?;
    let mappings = vec![
        (source.root.clone(), target.root.clone()),
        (canonical_source, target.root.clone()),
    ];
    let mut pending = workspaces
        .workspace_roots()
        .filter_map(|path| project(path, &mappings))
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    let mut metadata_roots = BTreeSet::new();
    let canonical_target = fs::canonicalize(&target.root).map_err(|e| e.to_string())?;
    let mut contexts = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        if !metadata.is_dir() || metadata_roots.contains(&path) || !seen.insert(path.clone()) {
            continue;
        }
        // Reuse the existing decoder, but only consume Git directories already
        // contained in the copy. Relocating source-selecting storage pointers
        // remains the storage owner's responsibility.
        // Native Git validates both ordinary .git entries and bare roots.
        // The cheap marker check avoids running Git in every copied directory.
        let entry = path.join(".git");
        let has_entry = fs::symlink_metadata(&entry).is_ok();
        let bare_candidate =
            !has_entry && path.join("HEAD").is_file() && path.join("objects").is_dir();
        if (has_entry || bare_candidate)
            && git_command()
                .args(["rev-parse", "--resolve-git-dir"])
                .arg(git_path(if has_entry { &entry } else { &path }))
                .output()
                .map_err(|e| e.to_string())?
                .status
                .success()
            && let Some(identity) = if bare_candidate {
                Some(git_registration_paths(&path)?)
            } else {
                git_identity_paths(&path)?
            }
            && let (Ok(private), Ok(common)) = (
                owned(&canonical_target, &identity.private_dir),
                owned(&canonical_target, &identity.common_dir),
            )
        {
            metadata_roots.insert(
                target
                    .root
                    .join(private.strip_prefix(&canonical_target).unwrap()),
            );
            metadata_roots.insert(
                target
                    .root
                    .join(common.strip_prefix(&canonical_target).unwrap()),
            );
            contexts.push(CopiedGitConfigContext {
                checkout: path.clone(),
                private_dir: identity.private_dir,
                common_dir: identity.common_dir,
            });
            if bare_candidate {
                continue; // The repository itself is administrative storage.
            }
        }
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() != ".git" && entry.file_type().map_err(|e| e.to_string())?.is_dir()
            {
                pending.push(entry.path());
            }
        }
    }
    Ok(contexts)
}

fn owned(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let resolved = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !resolved.starts_with(root) {
        return Err(format!(
            "copied Git configuration escapes the destination: {}",
            display_path(path)
        ));
    }
    Ok(resolved)
}

fn retain(root: &Path, path: &Path, retained: &mut BTreeSet<PathBuf>) -> Result<PathBuf, String> {
    let resolved = owned(root, path)?;
    if !resolved.is_file() {
        return Err("copied Git configuration is not a regular file".into());
    }
    retained.insert(resolved.clone());
    // Preserve aliases needed to reach a config without retaining their whole
    // target directory. Relative include resolution still uses the authored path.
    for entry in crate::store::dev_sources::existing_source_path_entries(path)? {
        if fs::symlink_metadata(&entry).is_ok_and(|m| m.is_symlink()) {
            let parent = fs::canonicalize(entry.parent().ok_or("invalid config alias")?)
                .map_err(|e| e.to_string())?;
            if parent.starts_with(root) {
                retained.insert(parent.join(entry.file_name().ok_or("invalid config alias")?));
            } else if !fs::canonicalize(&entry).is_ok_and(|target| root.starts_with(target)) {
                return Err("copied Git configuration traverses an unowned alias".into());
            }
        }
    }
    Ok(resolved)
}

#[derive(Clone)]
struct ConfigPath {
    key: Vec<u8>,
    index: usize,
    value: Option<PathBuf>,
}

type ConfigEdits = BTreeMap<Vec<u8>, Vec<(usize, PathBuf)>>;

struct EditedConfig {
    entries: Vec<ConfigPath>,
    edits: ConfigEdits,
}

fn read_config_paths(file: &Path) -> Result<Vec<ConfigPath>, String> {
    let output = git_command()
        .args(["config", "--file"])
        .arg(git_path(file))
        .args([
            "--no-includes",
            "--null",
            "--get-regexp",
            "^(core\\.worktree|include\\.path|includeif\\..*\\.path)$",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !output.status.success() {
        return Err("cannot read copied Git path configuration".into());
    }
    let mut counts = BTreeMap::<Vec<u8>, usize>::new();
    let mut entries = Vec::new();
    for entry in output.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let split = entry
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("invalid Git config value output")?;
        let key = &entry[..split];
        let index = counts.entry(key.to_vec()).or_default();
        let raw = &entry[split + 1..];
        // Repository setup uses core.worktree literally. Includes interpolate
        // home/prefix through Git; a bad inactive include must remain inactive.
        let value = if key != b"core.worktree"
            && (raw.starts_with(b"~") || raw.starts_with(b"%(prefix)/"))
        {
            let expanded = git_command()
                .args([
                    "config",
                    "--file",
                    "-",
                    "--null",
                    "--type=path",
                    "--default",
                ])
                .arg(git_path_from_bytes(raw)?)
                .args(["--get", "ocm.path"])
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            if expanded.status.success() {
                Some(PathBuf::from(git_path_from_bytes(
                    expanded
                        .stdout
                        .strip_suffix(&[0])
                        .unwrap_or(&expanded.stdout),
                )?))
            } else {
                None
            }
        } else {
            Some(PathBuf::from(git_path_from_bytes(raw)?))
        };
        entries.push(ConfigPath {
            key: key.to_vec(),
            index: *index,
            value,
        });
        *index += 1;
    }
    Ok(entries)
}

fn edit_config(
    root: &Path,
    state_root: &Path,
    path: &Path,
    private_dir: &Path,
    mappings: &[(PathBuf, PathBuf)],
    visited: &mut BTreeMap<PathBuf, EditedConfig>,
    retained: &mut BTreeSet<PathBuf>,
) -> Result<bool, String> {
    let file = retain(root, path, retained)?;
    let entries = match visited.get(&file) {
        Some(previous) => previous.entries.clone(),
        None => read_config_paths(&file)?,
    };
    let mut edits = ConfigEdits::new();
    for entry in &entries {
        let Some(value) = &entry.value else { continue };
        let base = if entry.key == b"core.worktree" {
            private_dir
        } else {
            path.parent().unwrap()
        };
        let target = project_value(value, base, root, mappings);
        if let Some(target) = &target {
            edits
                .entry(entry.key.clone())
                .or_default()
                .push((entry.index, target.clone()));
        }
        // Retain already-copied referenced files without parsing inactive
        // contents or making missing/inactive targets prerequisites.
        if entry.key != b"core.worktree" {
            let included = target.unwrap_or_else(|| base.join(value));
            if fs::canonicalize(&included).is_ok_and(|p| p.starts_with(root) && p.is_file()) {
                let mut optional = BTreeSet::new();
                if retain(root, &included, &mut optional).is_ok() {
                    retained.extend(optional);
                }
            }
        }
    }
    if !edits.is_empty() {
        if is_integrity_protected_history_destination(state_root, path, &file)? {
            return Err("copied Git configuration would modify integrity-protected history".into());
        }
        // Git locks the selected filename, which may be an alias. Check both
        // that name and the canonical file, including later selected aliases.
        for candidate in [path, file.as_path()] {
            let mut lock = candidate.as_os_str().to_os_string();
            lock.push(".lock");
            if fs::symlink_metadata(Path::new(&lock)).is_ok() {
                return Err(
                    "copied Git configuration is locked; resolve the lock before copying".into(),
                );
            }
        }
    }
    if let Some(previous) = visited.get(&file) {
        if previous.edits != edits {
            return Err("copied Git configuration has conflicting relative path contexts".into());
        }
        return Ok(false);
    }
    visited.insert(
        file.clone(),
        EditedConfig {
            entries,
            edits: edits.clone(),
        },
    );
    if edits.is_empty() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if fs::metadata(&file).map_err(|e| e.to_string())?.nlink() != 1 {
            return Err("copied Git configuration has shared hard-link ownership".into());
        }
    }
    let raw = fs::read(&file).map_err(|e| e.to_string())?;
    let mut config = gix_config::File::from_bytes_no_includes(
        &raw,
        gix_config::file::Metadata::default(),
        Default::default(),
    )
    .map_err(|_| "cannot parse copied Git configuration")?;
    for (key, edits) in edits {
        let mut values = if key == b"core.worktree" {
            config.raw_values_mut("core.worktree")
        } else if key == b"include.path" {
            config.raw_values_mut("include.path")
        } else {
            let condition = key
                .strip_prefix(b"includeif.")
                .and_then(|v| v.strip_suffix(b".path"))
                .ok_or("invalid conditional config key")?;
            config.raw_values_mut_by("includeif", Some(condition.as_bstr()), "path")
        }
        .map_err(|_| "cannot locate copied Git configuration value")?;
        for (index, target) in edits {
            if index >= values.len() {
                return Err("copied Git configuration occurrence mismatch".into());
            }
            values
                .set_at(index, git_path(&target).as_os_str().as_encoded_bytes())
                .map_err(|_| "cannot edit copied Git configuration value")?;
        }
    }
    let serialized = config.to_bstring();
    let mut result = Vec::with_capacity(serialized.len() + 3);
    if raw.starts_with(b"\xef\xbb\xbf") {
        result.extend_from_slice(b"\xef\xbb\xbf");
    }
    result.extend_from_slice(&serialized);
    with_owner_write_permission(&file, || {
        fs::write(&file, result).map_err(|e| e.to_string())
    })?;
    Ok(true)
}

// Resolve the containing directory, preserving the final alias: Git uses the
// include filename's spelling to resolve its own relative includes. Ownership
// checks separately resolve the complete path before any write.
fn project_value(
    value: &Path,
    copied_base: &Path,
    root: &Path,
    mappings: &[(PathBuf, PathBuf)],
) -> Option<PathBuf> {
    let selected = normalize_git_file_path(&copied_base.join(value));
    if !value.is_absolute() && selected.starts_with(root) {
        return None;
    }
    let projected = project(&selected, mappings)?;
    (git_path(&projected) != git_path(&selected)).then_some(projected)
}

fn project(path: &Path, mappings: &[(PathBuf, PathBuf)]) -> Option<PathBuf> {
    let path = git_path(path);
    mappings
        .iter()
        .filter_map(|(source, destination)| {
            let source = git_path(source);
            path.strip_prefix(&source)
                .ok()
                .map(|suffix| (source.components().count(), destination.join(suffix)))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, path)| path)
}

pub(super) fn git_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.as_os_str().to_string_lossy();
        let path = if let Some(path) = raw.strip_prefix(r"\\?\UNC\") {
            format!("//{path}")
        } else if let Some(path) = raw.strip_prefix(r"\\?\") {
            path.to_owned()
        } else {
            raw.into_owned()
        };
        // Git's relative gitfile resolver finds the parent using '/', including
        // on Windows. Keep aliases and '..' intact; only normalize separators.
        PathBuf::from(path.replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&std::ffi::OsStr]) {
        let output = git_command()
            .arg("-C")
            .arg(git_path(repo))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn copied_config_selection_restarts_before_stale_nested_origins() {
        use std::ffi::OsStr;
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let source = base.join("old-source");
        let target = source.join("nested");
        let checkout = target.join("nested/repo");
        fs::create_dir_all(&checkout).unwrap();
        git(&checkout, &[OsStr::new("init"), OsStr::new("-q")]);
        git(
            &checkout,
            &[
                OsStr::new("config"),
                OsStr::new("core.worktree"),
                git_path(&source.join("nested/repo")).as_os_str(),
            ],
        );
        git(
            &checkout,
            &[
                OsStr::new("config"),
                OsStr::new("include.path"),
                git_path(&source.join("nested/first")).as_os_str(),
            ],
        );
        let first = target.join("nested/first");
        git(
            &checkout,
            &[
                OsStr::new("config"),
                OsStr::new("--file"),
                git_path(&first).as_os_str(),
                OsStr::new("include.path"),
                git_path(&source.join("nested/decoy")).as_os_str(),
            ],
        );
        fs::write(
            target.join("nested/decoy"),
            "[fixture]\n value = selected\n",
        )
        .unwrap();
        let decoy = target.join("decoy");
        git(
            &checkout,
            &[
                OsStr::new("config"),
                OsStr::new("--file"),
                git_path(&decoy).as_os_str(),
                OsStr::new("core.worktree"),
                git_path(&source.join("unrelated")).as_os_str(),
            ],
        );
        let original = fs::read(&decoy).unwrap();
        let contexts = [CopiedGitConfigContext {
            checkout: checkout.clone(),
            private_dir: checkout.join(".git"),
            common_dir: checkout.join(".git"),
        }];
        let retained =
            relocate_copied_git_config(&target, &[(source, target.clone())], &contexts).unwrap();
        validate_copied_git_config(&target, &contexts).unwrap();
        assert_eq!(fs::read(&decoy).unwrap(), original);
        assert!(!retained.contains(&decoy));
        assert!(retained.contains(&target.join("nested/decoy")));
    }

    #[test]
    fn copied_config_identity_mapping_preserves_authored_bytes() {
        use std::ffi::OsStr;
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let checkout = root.join("repo");
        fs::create_dir(&checkout).unwrap();
        git(&checkout, &[OsStr::new("init"), OsStr::new("-q")]);
        let config = checkout.join(".git/config");
        let mut raw = fs::read(&config).unwrap();
        raw.extend_from_slice(
            format!(
                "[core]\n worktree = \"{}\" # retain spelling\n",
                git_path(&checkout).display()
            )
            .as_bytes(),
        );
        fs::write(&config, &raw).unwrap();
        let contexts = [CopiedGitConfigContext {
            checkout: checkout.clone(),
            private_dir: checkout.join(".git"),
            common_dir: checkout.join(".git"),
        }];
        relocate_copied_git_config(&root, &[(root.clone(), root.clone())], &contexts).unwrap();
        assert_eq!(fs::read(config).unwrap(), raw);
    }
}
