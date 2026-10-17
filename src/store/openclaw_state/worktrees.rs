use super::*;
use crate::openclaw_repo::{git_command, git_identity_paths, read_git_pointer};
use sha2::{Digest, Sha256};

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
    writes: BTreeMap<PathBuf, Vec<u8>>,
    configs: BTreeMap<PathBuf, PathBuf>,
    preserved: BTreeSet<PathBuf>,
}

pub(in crate::store) fn relocate_copied_managed_worktrees(
    paths: &EnvPaths,
    mappings: &[(&Path, &Path)],
) -> Result<BTreeSet<PathBuf>, String> {
    let Some(database) = contained_imported_database(
        &paths.state_dir.join("state/openclaw.sqlite"),
        &paths.root,
        true,
    )?
    else {
        return Ok(BTreeSet::new());
    };
    if !is_sqlite_database(&database)? {
        return Ok(BTreeSet::new());
    }
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    if !has_table(&db, "worktrees")? {
        return Ok(BTreeSet::new());
    }
    let root = fs::canonicalize(&paths.root).map_err(|error| error.to_string())?;
    let mut graph = CopyGraph {
        root,
        target: &paths.root,
        mappings,
        checkouts: BTreeMap::new(),
        commons: BTreeSet::new(),
        object_dirs: BTreeSet::new(),
        metadata_dirs: BTreeSet::new(),
        writes: BTreeMap::new(),
        configs: BTreeMap::new(),
        preserved: BTreeSet::new(),
    };
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
    drop(db);
    if records.is_empty() {
        return Ok(BTreeSet::new());
    }
    // The whole required graph has been resolved inside the destination before
    // changing any pointer. Never ask Git to repair source-selecting pointers.
    for (file, bytes) in &graph.writes {
        with_owner_write_permission(file, || {
            fs::write(file, bytes).map_err(|error| error.to_string())
        })?;
    }
    for (file, value) in &graph.configs {
        with_owner_write_permission(file.parent().ok_or("Git config has no parent")?, || {
            with_owner_write_permission(file, || {
                let output = git_command()
                    .args(["config", "--no-includes", "--file"])
                    .arg(file)
                    .arg("core.worktree")
                    .arg(value)
                    .output()
                    .map_err(|error| error.to_string())?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err("failed to relocate copied Git worktree configuration".to_string())
                }
            })
        })?;
    }
    for (path, expected) in &graph.checkouts {
        let actual = git_identity_paths(path)?.ok_or("copied Git checkout disappeared")?;
        if fs::canonicalize(actual.private_dir).map_err(|e| e.to_string())? != expected.private
            || fs::canonicalize(actual.common_dir).map_err(|e| e.to_string())? != expected.common
            || Path::new(&git_value(path, &["rev-parse", "--show-toplevel"])?) != path
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
            let digest = Sha256::digest(format!("{}\n{origin}", display_path(common)).as_bytes());
            fingerprints.insert(
                common.clone(),
                digest[..8]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            );
        }
    }
    with_owner_write_permission(database.parent().ok_or("database has no parent")?, || {
        with_owner_write_permission(&database, || {
            let mut db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)
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
                    rusqlite::params![display_path(&record.repo), display_path(&record.checkout), fingerprints[&graph.checkouts[&record.repo].common], record.id])
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())
        })
    })?;
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
        if path.starts_with(&self.root) {
            return Some(path.to_path_buf());
        }
        if let Ok(relative) = path.strip_prefix(self.target) {
            return Some(self.root.join(relative));
        }
        self.mappings
            .iter()
            .filter_map(|(source, target)| {
                path.strip_prefix(source)
                    .ok()
                    .map(|relative| (source.components().count(), target.join(relative)))
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, target)| target)
    }

    fn owned(&self, path: &Path) -> Result<PathBuf, String> {
        let projected = self.project(path).ok_or_else(|| format!("cannot copy managed worktree: required Git path is outside the copied environment: {}", display_path(path)))?;
        let actual = fs::canonicalize(&projected).map_err(|error| {
            format!(
                "cannot copy managed worktree: required Git path is unavailable: {}: {error}",
                display_path(&projected)
            )
        })?;
        if !actual.starts_with(&self.root) {
            return Err(format!(
                "cannot copy managed worktree: Git path escapes the copied environment: {}",
                display_path(&projected)
            ));
        }
        Ok(actual)
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
        let target = self.owned(&value)?;
        if clean_path(&value) != target {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(target.as_os_str().as_encoded_bytes());
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
        self.config_worktree(&private.join("config.worktree"))?;
        if self.commons.insert(common.clone()) {
            self.validate_metadata_links(&common)?;
            self.config_worktree(&common.join("config"))?;
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

    fn check_locks(&self, directory: &Path) -> Result<(), String> {
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
            self.check_ref_locks(&self.owned(&refs)?)?;
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

    fn config_worktree(&mut self, path: &Path) -> Result<(), String> {
        if !git_entry_exists(path)? {
            return Ok(());
        }
        let path = self.owned(path)?;
        let output = git_command()
            .args(["config", "--no-includes", "--file"])
            .arg(&path)
            .args(["--get", "core.worktree"])
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.code() == Some(1) {
            return Ok(());
        }
        if !output.status.success() {
            return Err("cannot read copied Git worktree configuration".to_string());
        }
        let value = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        let value = Path::new(value.trim_end_matches(['\r', '\n']));
        let original = if value.is_absolute() {
            value.to_path_buf()
        } else {
            path.parent().unwrap().join(value)
        };
        let target = self.owned(&original)?;
        if clean_path(&original) != target {
            self.configs.insert(path, target);
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
