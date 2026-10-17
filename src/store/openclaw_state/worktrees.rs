use super::git_config::{
    CopiedGitConfigContext, git_path, relocate_copied_git_config, validate_copied_git_config,
};
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MetadataScope {
    Root,
    Refs,
    Other,
}

/// Prepared destination state. Preserve its roots/leaves during cleanup, then
/// validate and publish managed identity exactly once with `finish`.
#[must_use]
pub(in crate::store) struct CopiedWorktrees {
    pub preserved_roots: BTreeSet<PathBuf>,
    pub config_paths: BTreeSet<PathBuf>,
    contexts: Vec<CopiedGitConfigContext>,
    checkouts: BTreeMap<PathBuf, Checkout>,
    records: Vec<CopiedWorktree>,
    database: Option<PathBuf>,
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
    metadata_dirs: BTreeSet<(PathBuf, MetadataScope)>,
    resolved_directories: BTreeMap<PathBuf, PathBuf>,
    resolved_aliases: BTreeMap<PathBuf, PathBuf>,
    writes: BTreeMap<PathBuf, Vec<u8>>,
    links: BTreeMap<PathBuf, (PathBuf, PathBuf)>,
    preserved: BTreeSet<PathBuf>,
}

/// Prepare copied storage and configuration; identity is finalized after cleanup.
/// Mappings describe existing copy operations; this never
/// acquires missing source data or expands a caller's copy selection.
pub(in crate::store) fn relocate_copied_worktrees(
    paths: &EnvPaths,
    mappings: &[(&Path, &Path)],
    mut contexts: Vec<CopiedGitConfigContext>,
) -> Result<CopiedWorktrees, String> {
    let database_path = paths.state_dir.join("state/openclaw.sqlite");
    let database =
        contained_imported_database(&database_path, &paths.root, &paths.state_dir, true)?;
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
        resolved_aliases: BTreeMap::new(),
        writes: BTreeMap::new(),
        links: BTreeMap::new(),
        preserved: BTreeSet::new(),
    };
    let records = read_managed_registry(database.as_deref(), &mut graph)?;
    if let Some(database) = database.as_ref().filter(|_| !records.is_empty())
        && graph.owned(&database_path)? != *database
    {
        return Err("copied managed registry changed during finalization".to_string());
    }
    // The copied footprint need not contain a state directory.
    let state_root = derive_env_paths(&graph.root).state_dir;
    let namespaces = integrity_protected_history_namespaces(&state_root)
        .into_iter()
        .filter_map(|logical| {
            graph
                .resolve_owned(&logical, 0, false)
                .ok()
                .map(|resolved| (logical, resolved))
        })
        .collect::<Vec<_>>();
    for path in graph
        .writes
        .keys()
        .chain(graph.links.keys())
        .chain(database.as_ref().filter(|_| !records.is_empty()))
    {
        if is_integrity_protected_history_destination_from_observations(
            &state_root,
            path,
            &graph.resolved_aliases,
            &namespaces,
        ) {
            return Err(format!(
                "cannot copy managed worktree: required Git relocation would modify integrity-protected history at {}",
                display_path(path)
            ));
        }
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
                std::os::windows::fs::symlink_dir(native_path(target).as_ref(), link)
                    .map_err(|error| error.to_string())?;
            } else {
                std::os::windows::fs::symlink_file(native_path(target).as_ref(), link)
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
    contexts.extend(
        graph
            .checkouts
            .iter()
            .map(|(checkout, identity)| CopiedGitConfigContext {
                checkout: checkout.clone(),
                private_dir: identity.private.clone(),
                common_dir: identity.common.clone(),
            }),
    );
    contexts.sort_by(|a, b| {
        (&a.checkout, &a.private_dir, &a.common_dir).cmp(&(
            &b.checkout,
            &b.private_dir,
            &b.common_dir,
        ))
    });
    contexts.dedup_by(|a, b| {
        a.checkout == b.checkout && a.private_dir == b.private_dir && a.common_dir == b.common_dir
    });
    let config_mappings = mappings
        .iter()
        .map(|(source, target)| (source.to_path_buf(), target.to_path_buf()))
        .collect::<Vec<_>>();
    let config_paths = relocate_copied_git_config(&paths.root, &config_mappings, &contexts)?;
    // Retention walks use the caller's root spelling, whereas Git identities
    // and fingerprints use its canonical spelling, as OpenClaw does.
    let preserved_roots = graph
        .preserved
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(&graph.root)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(|relative| paths.root.join(relative))
        })
        .collect();
    Ok(CopiedWorktrees {
        preserved_roots,
        config_paths,
        contexts,
        checkouts: graph.checkouts,
        records,
        database,
    })
}

impl CopiedWorktrees {
    /// Validate the prepared copy after cleanup. This never relocates again.
    pub(in crate::store) fn finish(self, paths: &EnvPaths) -> Result<(), String> {
        let root = fs::canonicalize(&paths.root).map_err(|error| error.to_string())?;
        for path in &self.config_paths {
            if !fs::canonicalize(path).is_ok_and(|path| path.starts_with(&root)) {
                return Err(format!(
                    "required copied Git configuration did not survive cleanup: {}",
                    display_path(path)
                ));
            }
        }
        validate_copied_git_config(&paths.root, &self.contexts)?;
        for (path, expected) in &self.checkouts {
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
        for record in &self.records {
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
            let common = &self.checkouts[&record.repo].common;
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
                let canonical_common =
                    fs::canonicalize(common).map_err(|error| error.to_string())?;
                let digest = Sha256::digest(
                    format!(
                        "{}\n{origin}",
                        display_path(&native_path(&canonical_common))
                    )
                    .as_bytes(),
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
        if let Some(database) = self.database.filter(|_| !self.records.is_empty()) {
            with_owner_write_permission(
                database.parent().ok_or("database has no parent")?,
                || {
                    with_owner_write_permission(&database, || {
                        let mut db = Connection::open_with_flags(
                            &database,
                            OpenFlags::SQLITE_OPEN_READ_WRITE,
                        )
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
                        for record in &self.records {
                            tx.execute("UPDATE worktrees SET repo_root = ?1, path = ?2, repo_fingerprint = ?3 WHERE id = ?4",
                    rusqlite::params![display_path(&native_path(&record.repo)), display_path(&native_path(&record.checkout)), fingerprints[&self.checkouts[&record.repo].common], record.id])
                    .map_err(|e| e.to_string())?;
                        }
                        tx.commit().map_err(|e| e.to_string())
                    })
                },
            )?;
        }
        Ok(())
    }
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
    let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
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

// OpenClaw stores native path spelling and hashes it into repository identity.
// Git's own text files use `git_path` instead, with slash separators on Windows.
fn native_path(path: &Path) -> Cow<'_, Path> {
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

// Git paths use C quoting, including three-digit octal byte escapes.
// A malformed quoted alternate is a literal path, matching Git's fallback.
fn unquote_git_path(input: &[u8]) -> Option<(Vec<u8>, usize)> {
    if input.first() != Some(&b'"') {
        return None;
    }
    let mut bytes = Vec::new();
    let mut cursor = 1;
    loop {
        let byte = *input.get(cursor)?;
        cursor += 1;
        match byte {
            b'"' => return Some((bytes, cursor)),
            b'\\' => {
                let escaped = *input.get(cursor)?;
                cursor += 1;
                bytes.push(match escaped {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => b'\r',
                    b'\\' | b'"' => escaped,
                    b'0'..=b'3' => {
                        let digits = input.get(cursor..cursor + 2)?;
                        if !digits.iter().all(|byte| matches!(byte, b'0'..=b'7')) {
                            return None;
                        }
                        cursor += 2;
                        ((escaped - b'0') << 6) | ((digits[0] - b'0') << 3) | (digits[1] - b'0')
                    }
                    _ => return None,
                });
            }
            0 => return None,
            _ => bytes.push(byte),
        }
    }
}

impl CopyGraph<'_> {
    fn project(&self, path: &Path) -> Option<PathBuf> {
        let path = native_path(path);
        if let Ok(relative) = path.strip_prefix(native_path(&self.root).as_ref()) {
            return Some(self.root.join(relative));
        }
        if let Ok(relative) = path.strip_prefix(native_path(self.target).as_ref()) {
            return Some(self.root.join(relative));
        }
        self.mappings
            .iter()
            .filter_map(|(source, target)| {
                path.strip_prefix(native_path(source).as_ref())
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
        self.resolve_owned(path, 0, true)
    }

    fn resolve_owned(
        &mut self,
        path: &Path,
        depth: usize,
        record: bool,
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
                let target = self.resolve_owned(&value, depth + 1, record)?;
                if record {
                    // Retain every encountered role, including relative file
                    // aliases that need no link rewrite. Cached directories
                    // keep these observations from their initial resolution.
                    self.resolved_aliases
                        .insert(current.clone(), target.clone());
                    if self.project(&value).as_deref() != Some(value.as_path()) {
                        self.links
                            .insert(current.clone(), (original, target.clone()));
                    }
                    self.preserved.insert(target.clone());
                    if target.is_dir() {
                        self.resolved_directories
                            .insert(current.clone(), target.clone());
                    }
                }
                current = target;
            } else if record && metadata.is_dir() {
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
        let is_backlink = file.file_name() == Some(OsStr::new("gitdir"));
        let file = self.owned(file)?;
        let value = read_git_pointer(&file, base, prefix)?.ok_or("Git pointer disappeared")?;
        let target = if is_backlink {
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
            // Git parses structural paths with slash separators, including on Windows.
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
            self.pointer(&root.join(".git"), &root, b"gitdir: ")?
                .ok_or("missing copied Git pointer")?
        };
        self.inspect_metadata(&private, MetadataScope::Root)?;
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
        if self.commons.insert(common.clone()) {
            self.inspect_metadata(&common, MetadataScope::Root)?;
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

    fn inspect_metadata(&mut self, directory: &Path, scope: MetadataScope) -> Result<(), String> {
        let directory = self.owned(directory)?;
        if !self.metadata_dirs.insert((directory.clone(), scope)) {
            return Ok(());
        }
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            // Hooks are authored executable configuration, not Git storage.
            let name = entry.file_name();
            if scope != MetadataScope::Refs && name == OsStr::new("hooks") {
                continue;
            }
            if (scope != MetadataScope::Other && name.to_string_lossy().ends_with(".lock"))
                || (scope == MetadataScope::Root && name == OsStr::new("locked"))
            {
                return Err(format!(
                    "cannot copy managed worktree: Git ownership is locked at {}; resolve the lock before copying",
                    display_path(&entry.path())
                ));
            }
            let path = self.owned(&entry.path())?;
            if path.is_dir() {
                let child_scope = match scope {
                    MetadataScope::Root if name == OsStr::new("refs") => MetadataScope::Refs,
                    MetadataScope::Refs => MetadataScope::Refs,
                    _ => MetadataScope::Other,
                };
                self.inspect_metadata(&path, child_scope)?;
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
        let original = fs::read(&file).map_err(|e| e.to_string())?;
        let body_end = original
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(original.len());
        let mut remaining = &original[..body_end];
        let mut updated = Vec::new();
        while !remaining.is_empty() {
            let end = remaining
                .iter()
                .position(|byte| matches!(byte, b'\n' | 0))
                .unwrap_or(remaining.len());
            if end == 0 || remaining[0] == b'#' {
                let consumed = (end + 1).min(remaining.len());
                updated.extend_from_slice(&remaining[..consumed]);
                remaining = &remaining[consumed..];
                continue;
            }
            let (mut raw, consumed) =
                unquote_git_path(remaining).unwrap_or_else(|| (remaining[..end].to_vec(), end));
            remaining = &remaining[(consumed + 1).min(remaining.len())..];
            if let Some(end) = raw.iter().position(|byte| *byte == 0) {
                raw.truncate(end);
            }
            if raw.is_empty() {
                updated.extend_from_slice(b"\"\"\n");
                continue;
            }
            #[cfg(unix)]
            let path = {
                use std::os::unix::ffi::OsStringExt;
                PathBuf::from(std::ffi::OsString::from_vec(raw))
            };
            #[cfg(not(unix))]
            let path = PathBuf::from(String::from_utf8(raw).map_err(|e| e.to_string())?);
            let path = if path.is_absolute() {
                path
            } else {
                objects.join(path)
            };
            let target = self.owned(&path)?;
            self.alternates(&target)?;
            updated.push(b'"');
            for byte in git_path(&target).as_os_str().as_encoded_bytes() {
                match byte {
                    b'"' | b'\\' => updated.extend_from_slice(&[b'\\', *byte]),
                    32..=126 => updated.push(*byte),
                    _ => updated.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
                }
            }
            updated.extend_from_slice(b"\"\n");
        }
        updated.extend_from_slice(&original[body_end..]);
        if updated != original {
            self.writes.insert(file, updated);
        }
        Ok(())
    }
}
