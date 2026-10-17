use super::*;
use crate::openclaw_repo::{git_command, git_identity_paths, read_git_pointer};
use sha2::{Digest, Sha256};
use std::borrow::Cow;

struct CopiedWorktree {
    id: String,
    repo: PathBuf,
    checkout: PathBuf,
}

#[derive(Clone)]
struct Checkout {
    private: PathBuf,
    common: PathBuf,
}

// Only structural Git pointers are changed. In particular, indexes, refs,
// snapshot data, authored configuration and checkout contents are not rebuilt.
struct CopyGraph<'a> {
    root: PathBuf,
    target: &'a Path,
    mappings: &'a [(&'a Path, &'a Path)],
    checkouts: BTreeMap<PathBuf, Checkout>,
    commons: BTreeSet<PathBuf>,
    object_dirs: BTreeSet<PathBuf>,
    metadata_dirs: BTreeSet<PathBuf>,
    resolved_directories: BTreeMap<PathBuf, PathBuf>,
    writes: BTreeMap<PathBuf, Vec<u8>>,
    links: BTreeMap<PathBuf, (PathBuf, PathBuf)>,
    configs: BTreeMap<(PathBuf, String, String), PathBuf>,
    config_sections: BTreeMap<(PathBuf, String), String>,
    config_files: BTreeSet<(PathBuf, PathBuf)>,
    preserved: BTreeSet<PathBuf>,
}

/// Finalize caller-selected copied Git roots and recognized managed rows.
/// Mappings describe existing copy operations; this never
/// acquires missing source data or expands a caller's copy selection.
pub(in crate::store) fn relocate_copied_worktrees(
    paths: &EnvPaths,
    mappings: &[(&Path, &Path)],
    copied_git_roots: &[PathBuf],
) -> Result<BTreeSet<PathBuf>, String> {
    let database = contained_imported_database(
        &paths.state_dir.join("state/openclaw.sqlite"),
        &paths.root,
        true,
    )?;
    let root = fs::canonicalize(&paths.root).map_err(|error| error.to_string())?;
    let mut graph = CopyGraph {
        root,
        target: &paths.root,
        mappings,
        checkouts: BTreeMap::new(),
        commons: BTreeSet::new(),
        object_dirs: BTreeSet::new(),
        metadata_dirs: BTreeSet::new(),
        resolved_directories: BTreeMap::new(),
        writes: BTreeMap::new(),
        links: BTreeMap::new(),
        configs: BTreeMap::new(),
        config_sections: BTreeMap::new(),
        config_files: BTreeSet::new(),
        preserved: BTreeSet::new(),
    };
    for root in copied_git_roots {
        graph.checkout(root)?;
    }
    let records = read_managed_registry(database.as_deref(), &mut graph)?;
    if graph.checkouts.is_empty() {
        return Ok(BTreeSet::new());
    }
    // The whole required graph has been resolved inside the destination before
    // changing any pointer. Never ask Git to repair source-selecting pointers.
    for (link, (original, target)) in &graph.links {
        with_owner_write_permission(link.parent().ok_or("Git link has no parent")?, || {
            if fs::read_link(link).map_err(|error| error.to_string())? != *original {
                return Err("copied Git link changed during finalization".to_string());
            }
            fs::remove_file(link).map_err(|error| error.to_string())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, link).map_err(|error| error.to_string())?;
            #[cfg(windows)]
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(git_path(target).as_ref(), link)
                    .map_err(|error| error.to_string())?;
            } else {
                std::os::windows::fs::symlink_file(git_path(target).as_ref(), link)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })?;
    }
    for (file, bytes) in &graph.writes {
        with_owner_write_permission(file, || {
            fs::write(file, bytes).map_err(|error| error.to_string())
        })?;
    }
    for ((file, key, original), value) in &graph.configs {
        let mut pattern = String::from("^");
        for character in original.chars() {
            if r"\.^$[](){}*+?|".contains(character) {
                pattern.push('\\');
            }
            pattern.push(character);
        }
        pattern.push('$');
        edit_git_config(
            file,
            &[
                "--replace-all",
                key,
                &display_path(&git_path(value)),
                &pattern,
            ],
        )?;
    }
    for ((file, original), replacement) in &graph.config_sections {
        rename_git_include_section(file, original, replacement)?;
    }
    for (path, expected) in &graph.checkouts {
        let actual = git_identity_paths(path)?.ok_or("copied Git checkout disappeared")?;
        if fs::canonicalize(actual.private_dir).map_err(|e| e.to_string())?
            != fs::canonicalize(&expected.private).map_err(|error| error.to_string())?
            || fs::canonicalize(actual.common_dir).map_err(|e| e.to_string())?
                != fs::canonicalize(&expected.common).map_err(|error| error.to_string())?
            || fs::canonicalize(git_value(path, &["rev-parse", "--show-toplevel"])?)
                .map_err(|error| error.to_string())?
                != fs::canonicalize(path).map_err(|error| error.to_string())?
        {
            return Err(format!(
                "copied Git checkout did not resolve to its destination: {}",
                display_path(path)
            ));
        }
    }
    let mut fingerprints = BTreeMap::new();
    for record in &records {
        let pending = git_command()
            .arg("-C")
            .arg(&record.repo)
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/openclaw/removals/{}", record.id))
            .output()
            .map_err(|error| error.to_string())?;
        if pending.status.code() != Some(1) {
            return Err(format!(
                "cannot copy managed worktree {}: resolve pending Git removal before copying",
                record.id
            ));
        }
        let common = &graph.checkouts[&record.repo].common;
        if !fingerprints.contains_key(common) {
            let output = git_command()
                .arg("-C")
                .arg(&record.repo)
                .args(["config", "--get", "remote.origin.url"])
                .output()
                .map_err(|e| e.to_string())?;
            if !output.status.success() && output.status.code() != Some(1) {
                return Err("failed to read copied repository origin".to_string());
            }
            let origin = std::str::from_utf8(&output.stdout)
                .map_err(|e| e.to_string())?
                .trim();
            let canonical_common = fs::canonicalize(common).map_err(|error| error.to_string())?;
            let digest = Sha256::digest(
                format!("{}\n{origin}", display_path(&git_path(&canonical_common))).as_bytes(),
            );
            fingerprints.insert(
                common.clone(),
                digest[..8]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            );
        }
    }
    if let Some(database) = database.filter(|_| !records.is_empty()) {
        with_owner_write_permission(database.parent().ok_or("database has no parent")?, || {
            with_owner_write_permission(&database, || {
                let mut db =
                    Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)
                        .map_err(|e| e.to_string())?;
                db.set_db_config(
                    rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY,
                    false,
                )
                .map_err(|e| e.to_string())?;
                db.set_db_config(
                    rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER,
                    false,
                )
                .map_err(|e| e.to_string())?;
                let tx = db.transaction().map_err(|e| e.to_string())?;
                for record in &records {
                    tx.execute("UPDATE worktrees SET repo_root = ?1, path = ?2, repo_fingerprint = ?3 WHERE id = ?4",
                    rusqlite::params![display_path(&git_path(&record.repo)), display_path(&git_path(&record.checkout)), fingerprints[&graph.checkouts[&record.repo].common], record.id])
                    .map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())
            })
        })?;
    }
    // Retention walks use the caller's root spelling, whereas Git identities
    // and fingerprints use its canonical spelling, as OpenClaw does.
    Ok(graph
        .preserved
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(&graph.root)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(|relative| paths.root.join(relative))
        })
        .collect())
}

fn read_managed_registry(
    database: Option<&Path>,
    graph: &mut CopyGraph<'_>,
) -> Result<Vec<CopiedWorktree>, String> {
    let Some(database) = database else {
        return Ok(Vec::new());
    };
    if !is_sqlite_database(database)? {
        return Ok(Vec::new());
    }
    let db = open_copied_database_readonly(database)?;
    if !has_table(&db, "worktrees")? {
        return Ok(Vec::new());
    }
    // Path-only historical tables remain the generic SQL relocation owner's
    // responsibility; they do not identify managed Git ownership.
    let identity_columns: u32 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('worktrees') WHERE name IN ('id', 'repo_root', 'path', 'repo_fingerprint')",
        [], |row| row.get(0),
    ).map_err(|error| error.to_string())?;
    if identity_columns != 4 {
        return Ok(Vec::new());
    }
    let has_removed: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('worktrees') WHERE name = 'removed_at')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let query = format!(
        "SELECT id, repo_root, path FROM worktrees{}",
        if has_removed {
            " WHERE removed_at IS NULL"
        } else {
            ""
        }
    );
    let rows = db
        .prepare(&query)
        .map_err(|error| error.to_string())?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let has_leases = has_table(&db, "state_leases")?;
    let mut records = Vec::new();
    for (id, repo, checkout) in rows {
        let repo = Path::new(&repo);
        let checkout = Path::new(&checkout);
        if graph.project(repo).is_none() && graph.project(checkout).is_none() {
            continue;
        }
        let repo = graph.owned(repo)?;
        let checkout = graph.owned(checkout)?;
        if has_leases {
            let leased: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_leases WHERE scope = ?1 OR (scope = 'core:managed-worktrees:mutation' AND lease_key = ?2))",
                    rusqlite::params![format!("worktree-run:{id}"), id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if leased {
                return Err(format!(
                    "cannot copy managed worktree {id}: retained run ownership must be resolved before copying"
                ));
            }
        }
        let repository = graph.checkout(&repo)?;
        let worktree = graph.checkout(&checkout)?;
        if repository.common != worktree.common {
            return Err(format!(
                "cannot copy managed worktree {id}: repository and checkout have different Git ownership"
            ));
        }
        records.push(CopiedWorktree { id, repo, checkout });
    }
    Ok(records)
}

fn edit_git_config(file: &Path, args: &[&str]) -> Result<(), String> {
    with_owner_write_permission(file.parent().ok_or("Git config has no parent")?, || {
        with_owner_write_permission(file, || {
            let output = git_command()
                .args(["config", "--no-includes", "--file"])
                .arg(file)
                .args(args)
                .output()
                .map_err(|error| error.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err("failed to relocate copied Git path configuration".to_string())
            }
        })
    })
}

fn rename_git_include_section(
    file: &Path,
    original: &str,
    replacement: &str,
) -> Result<(), String> {
    // Git reports section names folded to lowercase, but rename-section uses
    // the header's original spelling. Let Git parse and rewrite subsections;
    // inspect only the ASCII section-name tokens to supply that spelling.
    let bytes = fs::read(file).map_err(|error| error.to_string())?;
    let mut spellings = BTreeSet::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.trim_ascii_start();
        if let Some(header) = line.strip_prefix(b"[") {
            let name = header
                .iter()
                .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'-')
                .copied()
                .collect::<Vec<_>>();
            if name.eq_ignore_ascii_case(b"includeif") {
                spellings.insert(String::from_utf8(name).map_err(|error| error.to_string())?);
            }
        }
    }
    let old_suffix = original
        .strip_prefix("includeif.")
        .ok_or("invalid Git include section")?;
    let new_suffix = replacement
        .strip_prefix("includeif.")
        .ok_or("invalid Git include section")?;
    with_owner_write_permission(file.parent().ok_or("Git config has no parent")?, || {
        with_owner_write_permission(file, || {
            let mut renamed = false;
            for spelling in spellings {
                let output = git_command()
                    .args(["config", "--no-includes", "--file"])
                    .arg(file)
                    .args([
                        "--rename-section",
                        &format!("{spelling}.{old_suffix}"),
                        &format!("{spelling}.{new_suffix}"),
                    ])
                    .output()
                    .map_err(|error| error.to_string())?;
                renamed |= output.status.success();
            }
            let remaining = git_command()
                .args(["config", "--no-includes", "--file"])
                .arg(file)
                .args(["--get-all", &format!("{original}.path")])
                .output()
                .map_err(|error| error.to_string())?;
            if renamed && remaining.status.code() == Some(1) {
                Ok(())
            } else {
                Err(format!(
                    "cannot copy managed worktree: could not relocate Git include condition in {}",
                    display_path(file)
                ))
            }
        })
    })
}

fn git_path(path: &Path) -> Cow<'_, Path> {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return Cow::Owned(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return Cow::Owned(PathBuf::from(rest));
        }
    }
    Cow::Borrowed(path)
}

fn has_table(db: &Connection, table: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

fn git_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn git_value(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_command()
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "cannot validate copied Git checkout: {}",
            display_path(path)
        ));
    }
    Ok(String::from_utf8(output.stdout)
        .map_err(|e| e.to_string())?
        .trim_end_matches(['\r', '\n'])
        .to_string())
}

impl CopyGraph<'_> {
    fn project(&self, path: &Path) -> Option<PathBuf> {
        let path = git_path(path);
        if let Ok(relative) = path.strip_prefix(git_path(&self.root).as_ref()) {
            return Some(self.root.join(relative));
        }
        if let Ok(relative) = path.strip_prefix(git_path(self.target).as_ref()) {
            return Some(self.root.join(relative));
        }
        self.mappings
            .iter()
            .filter_map(|(source, target)| {
                path.strip_prefix(git_path(source).as_ref())
                    .ok()
                    .map(|relative| (source.components().count(), target.join(relative)))
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, target)| {
                target
                    .strip_prefix(self.target)
                    .map(|relative| self.root.join(relative))
                    .unwrap_or(target)
            })
    }

    fn owned(&mut self, path: &Path) -> Result<PathBuf, String> {
        self.resolve_owned(path, 0, false)
    }

    fn resolve_owned(
        &mut self,
        path: &Path,
        depth: usize,
        allow_missing: bool,
    ) -> Result<PathBuf, String> {
        if depth > 40 {
            return Err("cannot copy managed worktree: cyclic or excessive Git links".to_string());
        }
        let projected = self.project(path).ok_or_else(|| format!("cannot copy managed worktree: required Git path is outside the copied environment: {}", display_path(path)))?;
        let relative = projected
            .strip_prefix(&self.root)
            .or_else(|_| projected.strip_prefix(self.target))
            .map_err(|_| "cannot copy managed worktree: Git mapping leaves the destination")?;
        let mut current = self.root.clone();
        for component in relative.components() {
            match component {
                Component::CurDir => continue,
                Component::ParentDir if current != self.root => {
                    current.pop();
                    continue;
                }
                Component::Normal(name) => current.push(name),
                _ => {
                    return Err(
                        "cannot copy managed worktree: Git path escapes the copied environment"
                            .to_string(),
                    );
                }
            }
            if let Some(resolved) = self.resolved_directories.get(&current) {
                current = resolved.clone();
                continue;
            }
            let metadata = match fs::symlink_metadata(&current) {
                Ok(metadata) => metadata,
                Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "cannot copy managed worktree: required Git path is unavailable: {}: {error}",
                        display_path(&current)
                    ));
                }
            };
            if metadata.is_symlink() {
                let original = fs::read_link(&current).map_err(|error| error.to_string())?;
                let value = if original.is_absolute() {
                    original.clone()
                } else {
                    current.parent().unwrap().join(&original)
                };
                let target = self.resolve_owned(&value, depth + 1, allow_missing)?;
                if self.project(&value).as_deref() != Some(value.as_path()) {
                    self.links
                        .insert(current.clone(), (original, target.clone()));
                }
                self.preserved.insert(target.clone());
                if target.is_dir() {
                    self.resolved_directories
                        .insert(current.clone(), target.clone());
                }
                current = target;
            } else if metadata.is_dir() {
                self.resolved_directories
                    .insert(current.clone(), current.clone());
            }
        }
        Ok(current)
    }

    fn pointer(
        &mut self,
        file: &Path,
        base: &Path,
        prefix: &[u8],
    ) -> Result<Option<PathBuf>, String> {
        if !git_entry_exists(file)? {
            return Ok(None);
        }
        let file = self.owned(file)?;
        let value = read_git_pointer(&file, base, prefix)?.ok_or("Git pointer disappeared")?;
        let target = if file.file_name() == Some(OsStr::new("gitdir")) {
            // Git infers the checkout from the backlink's parent, so keep the
            // .git entry name even when that final entry is itself a symlink.
            let projected = self.project(&value).ok_or_else(|| format!("cannot copy managed worktree: Git backlink is outside the copied environment: {}", display_path(&value)))?;
            let parent = self.owned(projected.parent().ok_or("Git backlink has no parent")?)?;
            let entry = parent.join(projected.file_name().ok_or("Git backlink has no name")?);
            self.owned(&entry)?;
            entry
        } else {
            self.owned(&value)?
        };
        if clean_path(&value) != target {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(git_path(&target).as_os_str().as_encoded_bytes());
            bytes.push(b'\n');
            if let Some(previous) = self.writes.insert(file, bytes.clone())
                && previous != bytes
            {
                return Err("conflicting copied Git identity pointers".to_string());
            }
        }
        Ok(Some(target))
    }

    fn checkout(&mut self, root: &Path) -> Result<Checkout, String> {
        let root = self.owned(root)?;
        if let Some(existing) = self.checkouts.get(&root) {
            return Ok(existing.clone());
        }
        let entry = self.owned(&root.join(".git"))?;
        let private = if entry.is_dir() {
            entry
        } else {
            self.pointer(&entry, &root, b"gitdir: ")?
                .ok_or("missing copied Git pointer")?
        };
        self.check_locks(&private)?;
        let common = self
            .pointer(&private.join("commondir"), &private, b"")?
            .unwrap_or_else(|| private.clone());
        let backlink = self.pointer(&private.join("gitdir"), &private, b"")?;
        if private != common && backlink.as_deref() != Some(root.join(".git").as_path()) {
            return Err(format!(
                "cannot copy managed worktree: Git backlink does not match {}",
                display_path(&root)
            ));
        }
        let identity = Checkout {
            private: private.clone(),
            common: common.clone(),
        };
        self.checkouts.insert(root.clone(), identity.clone());
        self.preserved
            .extend([root.clone(), private.clone(), common.clone()]);
        self.config_paths(&private.join("config.worktree"), &private)?;
        if self.commons.insert(common.clone()) {
            self.validate_metadata_links(&common)?;
            self.config_paths(&common.join("config"), &common)?;
            self.check_locks(&common)?;
            self.alternates(&common.join("objects"))?;
            let slots = common.join("worktrees");
            if git_entry_exists(&slots)? {
                let slots = self.owned(&slots)?;
                for slot in fs::read_dir(slots).map_err(|e| e.to_string())? {
                    let slot = self.owned(&slot.map_err(|e| e.to_string())?.path())?;
                    let entry = self
                        .pointer(&slot.join("gitdir"), &slot, b"")?
                        .ok_or("copied Git registration has no backlink")?;
                    if entry.file_name() != Some(OsStr::new(".git")) {
                        return Err("invalid copied Git registration backlink".to_string());
                    }
                    let linked =
                        self.checkout(entry.parent().ok_or("Git backlink has no parent")?)?;
                    if linked.private != slot || linked.common != common {
                        return Err("inconsistent copied Git registration".to_string());
                    }
                }
            }
        }
        self.submodules(&root)?;
        Ok(identity)
    }

    fn submodules(&mut self, root: &Path) -> Result<(), String> {
        let config = root.join(".gitmodules");
        if !git_entry_exists(&config)? {
            return Ok(());
        }
        let config = self.owned(&config)?;
        let output = git_command()
            .args(["config", "--no-includes", "--null", "--file"])
            .arg(config)
            .args(["--get-regexp", "^submodule\\..*\\.path$"])
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.code() == Some(1) {
            return Ok(());
        }
        if !output.status.success() {
            return Err("cannot read copied Git submodule paths".to_string());
        }
        for entry in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let separator = entry
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or("invalid Git submodule path output")?;
            let value = &entry[separator + 1..];
            let relative = std::str::from_utf8(value).map_err(|error| error.to_string())?;
            let path = root.join(relative);
            // An uninitialized submodule has no checkout identity to relocate.
            if git_entry_exists(&path.join(".git"))? {
                self.checkout(&path)?;
            }
        }
        Ok(())
    }

    fn validate_metadata_links(&mut self, directory: &Path) -> Result<(), String> {
        let directory = self.owned(directory)?;
        if !self.metadata_dirs.insert(directory.clone()) {
            return Ok(());
        }
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            // Hooks are authored executable configuration, not Git storage.
            if entry.file_name() == OsStr::new("hooks") {
                continue;
            }
            let path = self.owned(&entry.path())?;
            if path.is_dir() {
                self.validate_metadata_links(&path)?;
            }
        }
        Ok(())
    }

    fn check_locks(&mut self, directory: &Path) -> Result<(), String> {
        for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            if name == OsStr::new("locked") || name.to_string_lossy().ends_with(".lock") {
                return Err(format!(
                    "cannot copy managed worktree: Git ownership is locked at {}; resolve the lock before copying",
                    display_path(&entry.path())
                ));
            }
        }
        let refs = directory.join("refs");
        if git_entry_exists(&refs)? {
            let refs = self.owned(&refs)?;
            self.check_ref_locks(&refs)?;
        }
        Ok(())
    }

    fn check_ref_locks(&self, directory: &Path) -> Result<(), String> {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry.file_name().to_string_lossy().ends_with(".lock") {
                return Err(format!(
                    "cannot copy managed worktree: resolve Git ref lock before copying: {}",
                    display_path(&entry.path())
                ));
            }
            if entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                self.check_ref_locks(&entry.path())?;
            }
        }
        Ok(())
    }

    fn config_paths(&mut self, path: &Path, git_dir: &Path) -> Result<(), String> {
        if !git_entry_exists(path)? {
            return Ok(());
        }
        let file = self.owned(path)?;
        if !self
            .config_files
            .insert((file.clone(), git_dir.to_path_buf()))
        {
            return Ok(());
        }
        let output = git_command()
            .args(["config", "--no-includes", "--null", "--file"])
            .arg(&file)
            .args([
                "--get-regexp",
                "^(core\\.worktree|include\\.path|includeif\\..*\\.path)$",
            ])
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.code() == Some(1) {
            return Ok(());
        }
        if !output.status.success() {
            return Err("cannot read copied Git path configuration".to_string());
        }
        let mut values = BTreeMap::<(String, String), usize>::new();
        for entry in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let split = entry
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or("invalid Git config output")?;
            let key = std::str::from_utf8(&entry[..split])
                .map_err(|error| error.to_string())?
                .to_string();
            let value = std::str::from_utf8(&entry[split + 1..])
                .map_err(|error| error.to_string())?
                .to_string();
            *values.entry((key, value)).or_default() += 1;
        }
        for ((key, raw), count) in values {
            let is_worktree = key == "core.worktree";
            // User/system and runtime-prefix includes keep their existing scope.
            if !is_worktree && (raw.starts_with('~') || raw.starts_with("%(prefix)")) {
                continue;
            }
            let value = Path::new(&raw);
            let base = if is_worktree {
                git_dir
            } else {
                file.parent().unwrap()
            };
            let original = if value.is_absolute() {
                value.to_path_buf()
            } else {
                base.join(value)
            };
            if !is_worktree && self.project(&original).is_none() {
                continue;
            }
            let target = self.resolve_owned(&original, 0, !is_worktree)?;
            if clean_path(&original) != target {
                if count != 1 {
                    return Err(format!(
                        "cannot copy managed worktree: repeated Git {key} entries require ordered relocation in {}; use equivalent relative paths for those repeated entries before copying",
                        display_path(&file)
                    ));
                }
                let identity = (file.clone(), key.clone(), raw);
                if let Some(previous) = self.configs.insert(identity, target.clone())
                    && previous != target
                {
                    return Err("cannot copy managed worktree: shared Git config has conflicting destinations".to_string());
                }
            }
            if !is_worktree {
                self.preserved.insert(target.clone());
                self.config_paths(&target, git_dir)?;
                if let Some(condition) = key
                    .strip_prefix("includeif.")
                    .and_then(|key| key.strip_suffix(".path"))
                {
                    for kind in ["gitdir:", "gitdir/i:", "worktree:", "worktree/i:"] {
                        if let Some(pattern) = condition.strip_prefix(kind) {
                            if Path::new(pattern).is_absolute()
                                && let Some(mapped) = self.project(Path::new(pattern))
                            {
                                let mut mapped = display_path(&git_path(&mapped));
                                if pattern.ends_with('/') && !mapped.ends_with('/') {
                                    mapped.push('/');
                                }
                                if pattern.ends_with('\\') && !mapped.ends_with('\\') {
                                    mapped.push('\\');
                                }
                                if mapped != pattern {
                                    self.config_sections.insert(
                                        (file.clone(), format!("includeif.{condition}")),
                                        format!("includeif.{kind}{mapped}"),
                                    );
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn alternates(&mut self, objects: &Path) -> Result<(), String> {
        let objects = self.owned(objects)?;
        if !self.object_dirs.insert(objects.clone()) {
            return Ok(());
        }
        self.preserved.insert(objects.clone());
        let file = objects.join("info/alternates");
        if !git_entry_exists(&file)? {
            return Ok(());
        }
        let file = self.owned(&file)?;
        let text = fs::read_to_string(&file).map_err(|e| e.to_string())?;
        let mut updated = String::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let raw: String = if line.starts_with('"') {
                serde_json::from_str(line)
                    .map_err(|e| format!("cannot interpret copied Git alternate path: {e}"))?
            } else {
                line.to_string()
            };
            let path = Path::new(&raw);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                objects.join(path)
            };
            let target = self.owned(&path)?;
            self.alternates(&target)?;
            updated.push_str(
                &serde_json::to_string(&display_path(&target)).map_err(|e| e.to_string())?,
            );
            updated.push('\n');
        }
        if updated != text {
            self.writes.insert(file, updated.into_bytes());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_git_graph_relocates_offline_without_managed_registry() {
        for path_only_table in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let temp_root = fs::canonicalize(temp.path()).unwrap();
            let source = temp_root.join("plain-home");
            let repository = temp_root.join("separate-workspace");
            let target = derive_env_paths(&temp_root.join("target"));
            fs::create_dir_all(&repository).unwrap();
            let git = |path: &Path, args: &[&str]| {
                let output = git_command()
                    .arg("-C")
                    .arg(path)
                    .args(args)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env(
                        "GIT_CONFIG_GLOBAL",
                        if cfg!(windows) { "NUL" } else { "/dev/null" },
                    )
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&repository, &["init", "-q"]);
            fs::write(repository.join("file"), "content").unwrap();
            git(&repository, &["add", "file"]);
            git(
                &repository,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-qm",
                    "fixture",
                ],
            );
            let checkout = source.join("worktrees/task");
            fs::create_dir_all(checkout.parent().unwrap()).unwrap();
            git(
                &repository,
                &["worktree", "add", "-qb", "task", checkout.to_str().unwrap()],
            );
            super::super::super::common::copy_dir_recursive(&source, &target.state_dir).unwrap();
            super::super::super::common::copy_dir_recursive(&repository, &target.workspace_dir)
                .unwrap();
            let db_path = target.state_dir.join("state/openclaw.sqlite");
            let before = if path_only_table {
                fs::create_dir_all(db_path.parent().unwrap()).unwrap();
                let db = Connection::open(&db_path).unwrap();
                db.execute_batch("CREATE TABLE worktrees (path TEXT); INSERT INTO worktrees VALUES ('historical path');").unwrap();
                drop(db);
                Some(fs::read(&db_path).unwrap())
            } else {
                None
            };
            fs::remove_dir_all(&source).unwrap();
            fs::remove_dir_all(&repository).unwrap();
            let retained = relocate_copied_worktrees(
                &target,
                &[
                    (&source, &target.state_dir),
                    (&repository, &target.workspace_dir),
                ],
                std::slice::from_ref(&target.workspace_dir),
            )
            .unwrap();
            let copied_checkout = target.state_dir.join("worktrees/task");
            let identity = git_identity_paths(&copied_checkout).unwrap().unwrap();
            assert_eq!(
                fs::canonicalize(identity.common_dir).unwrap(),
                fs::canonicalize(target.workspace_dir.join(".git")).unwrap()
            );
            assert!(retained.contains(&copied_checkout));
            git(&copied_checkout, &["status", "--porcelain"]);
            if let Some(before) = before {
                assert_eq!(fs::read(db_path).unwrap(), before);
            }
        }
    }
}
