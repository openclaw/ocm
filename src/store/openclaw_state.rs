use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use crate::env::EnvMeta;
use crate::infra::archive::{EnvArchiveEntryKind, EnvArchiveOptions};

use super::common::path_exists;
use super::layout::{EnvPaths, clean_path, derive_env_paths, display_path};
use super::openclaw_workspaces::{
    OpenClawCleanupPathError, OpenClawWorkspaceInventory, OpenClawWorkspaceRuntime,
    canonicalize_path_allow_missing, resolve_env_openclaw_cleanup_paths,
    resolve_env_openclaw_copy_paths, resolve_env_openclaw_workspaces,
};

#[derive(Clone, Debug)]
pub(crate) struct OpenClawStateAudit {
    pub issues: Vec<String>,
    pub repair_runtime_state: bool,
    pub cleanup_error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct MigratedRuntimeStateResult {
    pub changed: bool,
    pub external_plugin_ids: Vec<String>,
}

pub(crate) fn audit_openclaw_state(
    meta: &EnvMeta,
    known_envs: &[EnvMeta],
    env: &BTreeMap<String, String>,
) -> OpenClawStateAudit {
    let paths = derive_env_paths(Path::new(&meta.root));
    if !path_exists(&paths.state_dir) {
        return OpenClawStateAudit {
            issues: Vec::new(),
            repair_runtime_state: false,
            cleanup_error: None,
        };
    }
    let runtime = OpenClawWorkspaceRuntime::for_env(&meta.name, meta.gateway_port);
    let workspaces = match resolve_env_openclaw_cleanup_paths(&paths, env, runtime) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            let cleanup_error = match &error {
                OpenClawCleanupPathError::AgentSessionCollision(message) => Some(message.clone()),
                OpenClawCleanupPathError::Inventory(_) => None,
            };
            return OpenClawStateAudit {
                issues: vec![format!(
                    "OpenClaw workspace inventory could not be resolved safely: {}",
                    error.into_message()
                )],
                repair_runtime_state: false,
                cleanup_error,
            };
        }
    };

    let mut known_refs = BTreeMap::<String, (PathBuf, usize)>::new();
    let mut inferred_refs = BTreeMap::<PathBuf, usize>::new();
    collect_runtime_state_path_refs(
        &paths,
        &workspaces,
        meta,
        known_envs,
        &mut known_refs,
        &mut inferred_refs,
    );

    let mut issues = Vec::new();
    let known_roots = known_refs
        .values()
        .map(|(root, _)| root.clone())
        .collect::<BTreeSet<_>>();
    for (env_name, (root, count)) in &known_refs {
        push_issue(
            &mut issues,
            format!(
                "OpenClaw runtime state contains {count} copied path reference(s) under env \"{env_name}\" root: {}",
                display_path(root)
            ),
        );
    }
    for (root, count) in &inferred_refs {
        if known_roots.contains(root) {
            continue;
        }
        push_issue(
            &mut issues,
            format!(
                "OpenClaw runtime state contains {count} env-scoped path reference(s) outside the current env root: {}",
                display_path(root)
            ),
        );
    }

    OpenClawStateAudit {
        repair_runtime_state: !issues.is_empty(),
        issues,
        cleanup_error: None,
    }
}

pub(crate) fn repair_openclaw_runtime_state(
    meta: &EnvMeta,
    env: &BTreeMap<String, String>,
) -> Result<bool, String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    clear_nonportable_runtime_state(
        &paths,
        env,
        OpenClawWorkspaceRuntime::for_env(&meta.name, meta.gateway_port),
    )
}

pub(crate) fn openclaw_env_archive_options(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<EnvArchiveOptions, String> {
    let workspaces = resolve_env_openclaw_copy_paths(paths, env, runtime)?;
    let included_path_roots = workspaces.archive_relative_roots(&paths.root)?;
    let mut excluded_path_roots = openclaw_archive_excluded_path_roots(paths, false)?;
    excluded_path_roots.extend(openclaw_workspace_archive_excluded_path_roots(
        paths,
        &workspaces,
    )?);
    let excluded_path_roots =
        runtime_debris_outside_agent_directories(&paths.root, &workspaces, excluded_path_roots)?;
    Ok(EnvArchiveOptions {
        should_skip_path: should_skip_openclaw_env_archive_path,
        included_path_roots,
        excluded_path_roots,
        snapshot_sqlite_files: true,
        ..EnvArchiveOptions::default()
    })
}

pub(crate) fn openclaw_env_state_archive_options(
    paths: &EnvPaths,
) -> Result<EnvArchiveOptions, String> {
    // This mode archives files without interpreting OpenClaw configuration.
    // Do not follow linked directories while discovering rebuildable debris.
    let excluded_path_roots = if fs::symlink_metadata(&paths.state_dir)
        .is_ok_and(|meta| meta.is_dir())
        && fs::symlink_metadata(paths.state_dir.join("extensions")).is_ok_and(|meta| meta.is_dir())
    {
        openclaw_archive_excluded_path_roots(paths, true)?
    } else {
        BTreeSet::new()
    };
    Ok(EnvArchiveOptions {
        excluded_path_roots,
        should_exclude_path: should_exclude_state_archive_path,
        sqlite_content_roots: BTreeSet::from([PathBuf::new()]),
        snapshot_sqlite_files: true,
        relocate_internal_symlinks: true,
        private_output: true,
        ..EnvArchiveOptions::default()
    })
}

fn should_exclude_state_archive_path(path: &Path, kind: EnvArchiveEntryKind) -> bool {
    let name = path.file_name().unwrap_or_default();
    if path.parent() == Some(Path::new(".openclaw"))
        && kind == EnvArchiveEntryKind::Directory
        && matches!(
            name.to_str(),
            Some(
                "plugin-runtime-deps"
                    | "bundled-plugin-runtime-deps"
                    | ".openclaw-pnpm-store"
                    | ".openclaw-npm-cache"
            )
        )
    {
        return true;
    }
    kind == EnvArchiveEntryKind::Other
        || (path.parent() == Some(Path::new(".openclaw"))
            && super::snapshots::is_snapshot_process_residue(
                name,
                kind == EnvArchiveEntryKind::Directory,
            ))
        || (kind == EnvArchiveEntryKind::Directory && path.ends_with(".openclaw/tmp"))
}

#[cfg(test)]
pub(crate) fn openclaw_env_snapshot_archive_options(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<EnvArchiveOptions, String> {
    let mut options = openclaw_env_archive_options(paths, env, runtime)?;
    options
        .included_path_roots
        .insert(Path::new(".openclaw/state").to_path_buf());
    Ok(options)
}

pub(crate) fn should_skip_openclaw_env_archive_path(
    relative_path: &Path,
    _kind: EnvArchiveEntryKind,
) -> bool {
    let components = path_components(relative_path);
    if components.first().copied() != Some(".openclaw") {
        return false;
    }

    !is_durable_openclaw_archive_path(&components)
}

pub(crate) fn prepare_migrated_runtime_state(
    paths: &EnvPaths,
    source_state_root: &Path,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<MigratedRuntimeStateResult, String> {
    if !path_exists(&paths.state_dir) {
        return Ok(MigratedRuntimeStateResult::default());
    }
    let workspaces = resolve_archivable_workspaces(paths, env, runtime)?;

    let mut changed = false;
    changed |= rewrite_runtime_state_root_refs(
        &paths.state_dir,
        &paths.config_path,
        &workspaces,
        &[source_state_root],
        &paths.state_dir,
        false,
    )?;
    let plugin_paths = relocate_installed_plugin_index_paths(
        &paths.state_dir.join("state/openclaw.sqlite"),
        source_state_root,
        &paths.state_dir,
        PluginPathRelocation::Imported,
    )?;
    changed |= plugin_paths.changed;
    changed |= clear_volatile_runtime_state(
        &paths.state_dir,
        &paths.state_dir,
        &paths.config_path,
        &workspaces,
    )?;
    Ok(MigratedRuntimeStateResult {
        changed,
        external_plugin_ids: plugin_paths.external_plugin_ids,
    })
}

#[derive(Default)]
pub(super) struct ArchivedRuntimeStateResult {
    pub changed: bool,
    pub databases: BTreeMap<String, u64>,
}

pub(super) fn relocate_archived_runtime_state(
    paths: &EnvPaths,
    source_roots: &[&Path],
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<ArchivedRuntimeStateResult, String> {
    if source_roots.iter().any(|root| root.as_os_str().is_empty()) {
        return Err("archive source root is empty".to_string());
    }
    if !path_exists(&paths.state_dir) {
        return Ok(ArchivedRuntimeStateResult::default());
    }
    // Keep matching identities together: an unchanged longer root can prevent
    // a shorter canonical prefix from relocating the same reference again.
    let source_roots: &[&Path] = if source_roots.iter().all(|root| *root == paths.root) {
        &[]
    } else {
        source_roots
    };
    let changed = if source_roots.is_empty() {
        false
    } else {
        let workspaces = resolve_env_openclaw_workspaces(paths, env, runtime)?;
        rewrite_runtime_state_root_refs(
            &paths.state_dir,
            &paths.config_path,
            &workspaces,
            source_roots,
            &paths.root,
            true,
        )?
    };
    // Ownership admission still applies when no root relocation is needed or
    // the archive predates source-root metadata.
    let databases = relocate_copied_databases(
        paths,
        source_roots,
        DatabasePathRelocation::ArchivedState,
        None,
    )?;
    Ok(ArchivedRuntimeStateResult {
        changed: changed || databases.values().any(|count| *count != 0),
        databases,
    })
}

#[derive(Clone, Copy)]
pub(super) enum DatabasePathRelocation {
    ArchivedState,
    ClonedSessions,
}

pub(super) fn relocate_copied_databases(
    paths: &EnvPaths,
    source_roots: &[&Path],
    relocation: DatabasePathRelocation,
    configured_paths: Option<&OpenClawWorkspaceInventory>,
) -> Result<BTreeMap<String, u64>, String> {
    let mut counts = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let global = paths.state_dir.join("state/openclaw.sqlite");
    // Relocate registry paths before selecting additional agent databases. The
    // archive may be imported after the original environment has been removed.
    let global = contained_imported_database(&global, &paths.root, &paths.state_dir, true)?;
    if let Some(database) = &global {
        relocate_archived_database(
            paths,
            source_roots,
            relocation,
            database,
            &mut visited,
            &mut counts,
        )?;
    }
    let mut selection = OpenClawSqliteCopy {
        state_root: &paths.state_dir,
        registered_agents: BTreeSet::new(),
    };
    if let Some(database) = global {
        if is_sqlite_database(&database)? {
            selection.read_agent_registry(&database, true)?;
        }
    }
    // Default archives omit the global registry but retain configured agent
    // directories. Select their actual agent database, not arbitrary SQLite
    // payloads in the directory, and keep the usual history/containment checks.
    let mut configured_databases = Vec::new();
    if let Some(configured_paths) = configured_paths {
        for directory in configured_paths.agent_directory_roots() {
            // Retained configuration can name a plain file or a missing path.
            // Neither can contain an agent database; keep its existing data.
            match fs::metadata(directory) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            }
            let path = directory.join("openclaw-agent.sqlite");
            selection.registered_agents.insert(path.clone());
            configured_databases.push(path);
        }
    }
    // Admit every declared owner before traversal exclusions can hide a
    // configured or registered active database inside protected history.
    // Conflicts use the caller's existing copied-target rollback.
    selection.admit_registered_databases(&paths.root)?;
    for path in configured_databases {
        if let Some(database) =
            contained_imported_database(&path, &paths.root, &paths.state_dir, true)?
        {
            relocate_archived_database(
                paths,
                source_roots,
                relocation,
                &database,
                &mut visited,
                &mut counts,
            )?;
        }
    }
    let mut directories = vec![paths.root.clone()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                directories.push(path);
            } else if selection.contains(&path) {
                if let Some(database) =
                    contained_imported_database(&path, &paths.root, &paths.state_dir, true)?
                {
                    relocate_archived_database(
                        paths,
                        source_roots,
                        relocation,
                        &database,
                        &mut visited,
                        &mut counts,
                    )?;
                }
            }
        }
    }
    Ok(counts)
}

fn relocate_archived_database(
    paths: &EnvPaths,
    source_roots: &[&Path],
    relocation: DatabasePathRelocation,
    database: &Path,
    visited: &mut BTreeSet<PathBuf>,
    counts: &mut BTreeMap<String, u64>,
) -> Result<(), String> {
    if source_roots.is_empty() {
        return Ok(());
    }
    if !visited.insert(database.to_path_buf()) || !is_sqlite_database(database)? {
        return Ok(());
    }
    let parent = database.parent().ok_or("imported database has no parent")?;
    let count = with_owner_write_permission(parent, || {
        with_owner_write_permission(database, || {
            rewrite_database_root_references(
                database,
                source_roots,
                &display_path(&paths.root),
                relocation,
            )
        })
    })?;
    let root = fs::canonicalize(&paths.root).map_err(|error| error.to_string())?;
    let relative = database
        .strip_prefix(root)
        .map_err(|error| error.to_string())?;
    counts.insert(display_path(relative), count);
    Ok(())
}

fn quote_sql_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn rewrite_database_root_references(
    database: &Path,
    source_roots: &[&Path],
    target_root: &str,
    relocation: DatabasePathRelocation,
) -> Result<u64, String> {
    let mut source_roots = source_roots
        .iter()
        .map(|root| display_path(root))
        .collect::<Vec<_>>();
    // Match a nested lexical alias before its shorter canonical root.
    source_roots.sort_by_key(|root| std::cmp::Reverse(root.len()));
    source_roots.dedup();
    let rewrite = || -> Result<u64, Box<dyn std::error::Error>> {
        let mut db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        // Stored callbacks and cascades must not modify unrelated values.
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY,
            false,
        )?;
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER,
            false,
        )?;
        let tx = db.transaction()?;
        let schema = tx.prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'table' AND name NOT GLOB 'sqlite_*' ORDER BY name")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        // Update logical FTS text before its external content tables, while old
        // values are still available to remove their index entries. This avoids
        // rewriting binary backing tables or rebuilding unchanged indexes.
        let full_text = schema
            .iter()
            .filter_map(|(name, sql)| {
                let sql = sql
                    .to_ascii_lowercase()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                (sql.starts_with("create virtual table") && sql.contains("using fts5"))
                    .then_some(name.clone())
            })
            .collect::<BTreeSet<_>>();
        let shadows = full_text
            .iter()
            .flat_map(|name| {
                ["data", "idx", "content", "docsize", "config"]
                    .map(|suffix| format!("{name}_{suffix}"))
            })
            .collect::<BTreeSet<_>>();
        let mut tables = full_text.iter().cloned().collect::<Vec<_>>();
        tables.extend(
            schema
                .into_iter()
                .filter_map(|(name, sql)| {
                    (!sql.to_ascii_lowercase().starts_with("create virtual table")
                        && !shadows.contains(&name))
                    .then_some(name)
                })
                .collect::<Vec<_>>(),
        );
        tx.execute_batch("CREATE TEMP TABLE ocm_root_rewrites (column_id INTEGER, original TEXT COLLATE BINARY, replacement TEXT NOT NULL, PRIMARY KEY(column_id, original)) WITHOUT ROWID")?;
        let mut changed = 0;
        for table in tables {
            // Board rows are authored content and its bound checksums/metadata,
            // not runtime locations. Preserve them together, including paths
            // quoted in widget source, descriptors, titles, or declarations.
            if table.eq_ignore_ascii_case("board_tabs")
                || table.eq_ignore_ascii_case("board_widgets")
            {
                continue;
            }
            let sessions_only = matches!(relocation, DatabasePathRelocation::ClonedSessions);
            if sessions_only
                && !matches!(
                    table.as_str(),
                    "session_nodes" | "session_entries" | "agent_databases"
                )
            {
                continue;
            }
            // Older agent databases store live entries here; OpenClaw migrates
            // their entry_json directly into session_nodes on first open.
            let session_entry =
                sessions_only && matches!(table.as_str(), "session_nodes" | "session_entries");
            let columns = tx
                .prepare(
                    "SELECT cid, name FROM pragma_table_xinfo(?1) WHERE hidden = 0 ORDER BY cid",
                )?
                .query_map([&table], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|(_, name)| {
                    !sessions_only || name == if session_entry { "entry_json" } else { "path" }
                })
                .collect::<Vec<_>>();
            let table = quote_sql_identifier(&table);
            let quoted = columns
                .iter()
                .map(|(_, name)| quote_sql_identifier(name))
                .collect::<Vec<_>>();
            let mut changed_columns = BTreeSet::new();
            if !quoted.is_empty() {
                // Scan each table once. TEMP storage retains only changed text,
                // compared with BINARY collation across database text encodings.
                let mut read =
                    tx.prepare(&format!("SELECT {} FROM main.{table}", quoted.join(", ")))?;
                let mut rows = read.query([])?;
                while let Some(row) = rows.next()? {
                    for (index, (id, name)) in columns.iter().enumerate() {
                        let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(index)? else {
                            continue;
                        };
                        let original = std::str::from_utf8(bytes)?;
                        let path_column =
                            matches!(name.as_str(), "path" | "cwd" | "source" | "workspace")
                                || ["Path", "Dir", "Root", "_path", "_dir", "_root"]
                                    .iter()
                                    .any(|suffix| name.ends_with(suffix));
                        let replacement = if session_entry {
                            rewrite_cloned_session_entry(
                                original,
                                &source_roots,
                                target_root,
                                false,
                            )?
                        } else if path_column
                            && (Path::new(original).is_absolute()
                                || original
                                    .get(..5)
                                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:")))
                        {
                            rewrite_path_value(original, &source_roots, target_root)
                        } else {
                            rewrite_runtime_root_references(
                                original,
                                &source_roots,
                                target_root,
                                false,
                            )?
                        };
                        if replacement == original {
                            continue;
                        }
                        tx.execute(
                            "INSERT OR IGNORE INTO temp.ocm_root_rewrites VALUES (?1, ?2, ?3)",
                            rusqlite::params![id, original, replacement],
                        )?;
                        changed += 1;
                        changed_columns.insert(index);
                    }
                }
            }
            let mut assignments = Vec::new();
            let mut predicates = Vec::new();
            for index in changed_columns {
                let id = columns[index].0;
                let column = &quoted[index];
                let lookup = format!(
                    "SELECT replacement FROM temp.ocm_root_rewrites WHERE column_id = {id} AND original = ocm_source.{column} COLLATE BINARY AND typeof(ocm_source.{column}) = 'text'"
                );
                assignments.push(format!(
                    "{column} = COALESCE(({lookup}), ocm_source.{column})"
                ));
                predicates.push(format!("EXISTS ({lookup})"));
            }
            if !assignments.is_empty() {
                // One statement uses the original values for every column and
                // supports composite keys and WITHOUT ROWID tables. Never honor
                // a stored REPLACE policy by deleting a conflicting record.
                tx.execute(
                    &format!(
                        "UPDATE OR ABORT main.{table} AS ocm_source SET {} WHERE {}",
                        assignments.join(", "),
                        predicates.join(" OR ")
                    ),
                    [],
                )?;
            }
            tx.execute("DELETE FROM temp.ocm_root_rewrites", [])?;
        }
        tx.commit()?;
        Ok(changed)
    };
    rewrite().map_err(|error| {
        format!(
            "failed to relocate imported OpenClaw state database {}: {error}",
            display_path(database)
        )
    })
}

// Clone retains arbitrary user/plugin payloads, including executable content
// whose approval is bound to its exact bytes. Only session location metadata
// is relocatable here; splice its strings without reserializing other fields.
fn rewrite_cloned_session_entry(
    raw: &str,
    source_roots: &[String],
    target_root: &str,
    worktree: bool,
) -> Result<String, String> {
    if !raw.trim_start().starts_with('{')
        || serde_json::from_str::<serde::de::IgnoredAny>(raw).is_err()
    {
        return Ok(raw.to_string());
    }
    let mut output = String::with_capacity(raw.len());
    let mut cursor = raw.find('{').unwrap() + 1;
    output.push_str(&raw[..cursor]);
    loop {
        let tail = raw[cursor..].trim_start_matches(|ch: char| ch.is_whitespace() || ch == ',');
        if tail.starts_with('}') {
            output.push_str(&raw[cursor..]);
            break;
        }
        let key_start = raw.len() - tail.len();
        let key_end = json_value_end(raw, key_start)?;
        let key: String =
            serde_json::from_str(&raw[key_start..key_end]).map_err(|error| error.to_string())?;
        let colon = key_end
            + raw[key_end..]
                .find(':')
                .ok_or("missing session field separator")?;
        let value_start = raw.len() - raw[colon + 1..].trim_start().len();
        let value_end = json_value_end(raw, value_start)?;
        let value = &raw[value_start..value_end];
        output.push_str(&raw[cursor..value_start]);
        let path_field = if worktree {
            matches!(key.as_str(), "repoRoot" | "canonicalWorkspaceDir")
        } else {
            matches!(
                key.as_str(),
                "sessionRoot" | "spawnedCwd" | "spawnedWorkspaceDir"
            )
        };
        if !worktree && key == "worktree" {
            output.push_str(&rewrite_cloned_session_entry(
                value,
                source_roots,
                target_root,
                true,
            )?);
        } else if path_field && let Ok(path) = serde_json::from_str::<String>(value) {
            let replacement = rewrite_path_value(&path, source_roots, target_root);
            if replacement != path {
                output.push_str(
                    &serde_json::to_string(&replacement).map_err(|error| error.to_string())?,
                );
            } else {
                output.push_str(value);
            }
        } else {
            output.push_str(value);
        }
        cursor = value_end;
    }
    Ok(output)
}

pub(crate) fn validate_nonportable_runtime_state_paths(paths: &EnvPaths) -> Result<(), String> {
    // Check parents before descendants, including dangling links. These are the
    // directories cleanup opens directly rather than reaching via typed entries.
    for path in [
        paths.state_dir.clone(),
        paths.state_dir.join("agents"),
        paths.state_dir.join("extensions"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "cannot clear copied OpenClaw runtime state: expected a real directory, not a symlink or other file: {}",
                    display_path(&path)
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

pub(crate) fn clear_copied_runtime_state(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<bool, String> {
    clear_runtime_state_with_policy(paths, env, runtime, true)
}

pub(crate) fn clear_nonportable_runtime_state(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<bool, String> {
    clear_runtime_state_with_policy(paths, env, runtime, false)
}

fn clear_runtime_state_with_policy(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
    copying: bool,
) -> Result<bool, String> {
    validate_nonportable_runtime_state_paths(paths)?;
    if !path_exists(&paths.state_dir) {
        return Ok(false);
    }
    let workspaces = if copying {
        let inventory = resolve_env_openclaw_copy_paths(paths, env, runtime)?;
        inventory.archive_relative_roots(&paths.root)?;
        inventory
    } else {
        resolve_env_openclaw_cleanup_paths(paths, env, runtime)
            .map_err(OpenClawCleanupPathError::into_message)?
    };

    let mut changed = false;
    for relative_path in runtime_debris_outside_agent_directories(
        &paths.state_dir,
        &workspaces,
        openclaw_extension_runtime_debris_roots(&paths.state_dir)?,
    )? {
        remove_path(&paths.state_dir.join(relative_path))?;
        changed = true;
    }
    let entries = fs::read_dir(&paths.state_dir).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        if name == OsStr::new("openclaw.json")
            || is_durable_openclaw_state_root(&name)
            || workspaces.contains(&path)
            || is_integrity_protected_history_path(&paths.state_dir, &path)
        {
            continue;
        }
        if name == OsStr::new("agents") {
            if prune_agent_runtime_state(&path, &workspaces)? {
                changed = true;
            }
            continue;
        }
        if workspaces.has_descendant(&path) {
            changed |= clear_parent_preserving_workspaces(&path, &workspaces)?;
            continue;
        }

        remove_path(&path)?;
        changed = true;
    }

    Ok(changed)
}

fn is_durable_openclaw_state_root(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("state") | Some("plugins") | Some("extensions") | Some("npm") | Some("git")
    )
}

fn rewrite_runtime_state_root_refs(
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
    source_state_roots: &[&Path],
    target_state_root: &Path,
    archived: bool,
) -> Result<bool, String> {
    let mut changed = false;
    let source_roots = source_state_roots
        .iter()
        .map(|root| display_path(root))
        .collect::<Vec<_>>();
    let target_root = display_path(target_state_root);
    rewrite_runtime_state_root_refs_inner(
        root,
        root,
        config_path,
        workspaces,
        &source_roots,
        &target_root,
        archived,
        &mut changed,
    )?;
    Ok(changed)
}

fn rewrite_runtime_state_root_refs_inner(
    state_root: &Path,
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
    source_roots: &[String],
    target_root: &str,
    archived: bool,
    changed: &mut bool,
) -> Result<(), String> {
    if !path_exists(root) {
        return Ok(());
    }

    // DirEntry can retain its ReadDir handle. Collect paths before recursing.
    let paths = fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    for path in paths {
        if path == config_path || workspaces.contains(&path) {
            continue;
        }

        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            rewrite_runtime_state_root_refs_inner(
                state_root,
                &path,
                config_path,
                workspaces,
                source_roots,
                target_root,
                archived,
                changed,
            )?;
            continue;
        }
        if !metadata.is_file() || is_integrity_protected_history_path(state_root, &path) {
            continue;
        }

        if archived && is_sqlite_database(&path)? {
            // SQLite values are handled through their owning table, never as text.
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        // Retained records keep their format when backup, reset, deletion or
        // migration suffixes are appended to the original filename.
        let json_lines = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                name.split('.')
                    .any(|part| matches!(part, "jsonl" | "ndjson"))
            });
        let rewritten =
            rewrite_runtime_root_references(&raw, source_roots, target_root, json_lines)?;
        if rewritten == raw {
            continue;
        }
        let write = || fs::write(&path, rewritten).map_err(|error| error.to_string());
        if archived {
            with_owner_write_permission(&path, write)?;
        } else {
            write()?;
        }
        *changed = true;
    }

    Ok(())
}

fn rewrite_runtime_root_references(
    raw: &str,
    source_roots: &[String],
    target_root: &str,
    json_lines: bool,
) -> Result<String, String> {
    if !json_lines {
        let document = serde_json::from_str::<serde::de::IgnoredAny>(raw);
        if document.is_ok() {
            return rewrite_root_reference_value(raw, source_roots, target_root);
        }
        if document.is_err_and(|error| error.is_eof())
            && raw.trim_start().starts_with(['{', '[', '"'])
        {
            return Ok(raw.to_string());
        }
    }
    // Frame serialized records here, never after decoding a JSON string.
    // Contiguous legacy text remains a whole value, including quoted newlines.
    let mut output = String::with_capacity(raw.len());
    let mut text_start = 0;
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        let parsed = serde_json::from_str::<serde::de::IgnoredAny>(line);
        let complete = parsed.is_ok();
        let incomplete = !line.trim().is_empty() && parsed.is_err_and(|error| error.is_eof());
        if complete || incomplete {
            output.push_str(&rewrite_rooted_text(
                &raw[text_start..offset],
                source_roots,
                target_root,
            ));
            if complete {
                output.push_str(&rewrite_root_reference_value(
                    line,
                    source_roots,
                    target_root,
                )?);
            } else {
                output.push_str(line);
            }
            text_start = offset + line.len();
        }
        offset += line.len();
    }
    output.push_str(&rewrite_rooted_text(
        &raw[text_start..],
        source_roots,
        target_root,
    ));
    Ok(output)
}

fn rewrite_root_reference_value(
    raw: &str,
    source_roots: &[String],
    target_root: &str,
) -> Result<String, String> {
    if serde_json::from_str::<serde::de::IgnoredAny>(raw).is_ok() {
        // Preserve authored JSON bytes around the relocated strings, including
        // whitespace and escapes. Decoding also handles Windows path separators.
        let mut output = String::with_capacity(raw.len());
        let mut cursor = 0;
        let mut previous_key = None;
        while let Some(offset) = raw[cursor..].find('"') {
            let start = cursor + offset;
            output.push_str(&raw[cursor..start]);
            let end = json_value_end(raw, start)?;
            let is_key = raw[end..].trim_start().starts_with(':');
            let path_value = !is_key
                && previous_key.take().is_some_and(|key: String| {
                    matches!(
                        key.as_str(),
                        "path" | "source" | "setupSource" | "cwd" | "workspace" | "package"
                    ) || key.ends_with("Path")
                        || key.ends_with("Dir")
                        || key.ends_with("_path")
                        || key.ends_with("_dir")
                });
            let Ok(value) = serde_json::from_str::<String>(&raw[start..end]) else {
                if path_value || is_key {
                    output.push_str(&raw[start..end]);
                } else {
                    output.push_str(&rewrite_opaque_json_string(
                        &raw[start..end],
                        source_roots,
                        target_root,
                    )?);
                }
                cursor = end;
                previous_key = None;
                continue;
            };
            if is_key {
                previous_key = Some(value.clone());
            }
            let replacement = if path_value
                && (Path::new(&value).is_absolute()
                    || value
                        .get(..5)
                        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:")))
            {
                // A JSON path is one filename; whitespace is not an argument
                // separator, including escaped newlines within that filename.
                rewrite_path_value(&value, source_roots, target_root)
            } else {
                rewrite_root_reference_value(&value, source_roots, target_root)?
            };
            if replacement != value {
                output.push_str(
                    &serde_json::to_string(&replacement).map_err(|error| error.to_string())?,
                );
            } else {
                output.push_str(&raw[start..end]);
            }
            cursor = end;
        }
        output.push_str(&raw[cursor..]);
        return Ok(output);
    }
    if raw.contains('\n')
        && raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| serde_json::from_str::<serde::de::IgnoredAny>(line).is_ok())
        && raw
            .lines()
            .find(|line| !line.trim().is_empty())
            .is_some_and(|line| {
                line.trim_start().starts_with(['{', '[', '"'])
                    && serde_json::from_str::<serde::de::IgnoredAny>(line).is_ok()
            })
    {
        // Nested content consists entirely of complete serialized values.
        // Decode those values directly; file-level recovery framing stays above.
        let mut output = String::with_capacity(raw.len());
        for line in raw.split_inclusive('\n') {
            output.push_str(&rewrite_root_reference_value(
                line,
                source_roots,
                target_root,
            )?);
        }
        return Ok(output);
    }
    Ok(rewrite_rooted_text(raw, source_roots, target_root))
}

fn rewrite_opaque_json_string(
    raw: &str,
    source_roots: &[String],
    target_root: &str,
) -> Result<String, String> {
    let bytes = raw.as_bytes();
    let utf16_escape = |at: usize| -> Option<u16> {
        if bytes.get(at..at + 2)? != b"\\u" {
            return None;
        }
        u16::from_str_radix(raw.get(at + 2..at + 6)?, 16).ok()
    };
    let mut opaque = Vec::new();
    let mut scan = 1;
    while scan < raw.len() - 1 {
        if bytes[scan] == b'\\' {
            if let Some(escaped) = utf16_escape(scan) {
                if (0xd800..=0xdbff).contains(&escaped)
                    && utf16_escape(scan + 6).is_some_and(|low| (0xdc00..=0xdfff).contains(&low))
                {
                    scan += 12;
                    continue;
                }
                if (0xd800..=0xdfff).contains(&escaped) {
                    opaque.push(scan..scan + 6);
                }
                scan += 6;
            } else {
                scan += 2;
            }
        } else {
            scan += raw[scan..].chars().next().unwrap().len_utf8();
        }
    }
    let mut output = String::from("\"");
    let mut start = 1;
    let count = opaque.len();
    for (index, range) in opaque
        .into_iter()
        .chain([raw.len() - 1..raw.len() - 1])
        .enumerate()
    {
        let encoded = &raw[start..range.start];
        let decoded: String =
            serde_json::from_str(&format!("\"{encoded}\"")).map_err(|error| error.to_string())?;
        // Opaque UTF-16 escapes are not separators or valid path characters. Guards
        // keep a reference from acquiring a false boundary at a split.
        let guarded = format!(
            "{}{}{}",
            if start > 1 { "\0" } else { "" },
            decoded,
            if index < count { "\0" } else { "" }
        );
        let rewritten = rewrite_rooted_text(&guarded, source_roots, target_root);
        let rewritten =
            &rewritten[usize::from(start > 1)..rewritten.len() - usize::from(index < count)];
        if rewritten == decoded {
            output.push_str(encoded);
        } else {
            let encoded = serde_json::to_string(rewritten).map_err(|error| error.to_string())?;
            output.push_str(&encoded[1..encoded.len() - 1]);
        }
        output.push_str(&raw[range.clone()]);
        start = range.end;
    }
    output.push('"');
    Ok(output)
}

// Windows accepts both separators. Normalize only the comparison view so the
// original path suffix and unrelated text retain their spelling.
fn root_comparison(value: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(windows) {
        std::borrow::Cow::Owned(value.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(value)
    }
}

fn rooted_path_suffix<'a>(path: &'a str, source_root: &str) -> Option<std::borrow::Cow<'a, str>> {
    if path.contains('\0') {
        return None;
    }
    let path_comparison = root_comparison(path);
    let source_comparison = root_comparison(source_root);
    path_comparison.strip_prefix(source_comparison.as_ref())?;
    let suffix = &path[source_root.len()..];
    if !suffix.is_empty() && !suffix.starts_with(std::path::is_separator) {
        return None;
    }
    let mut depth = 0usize;
    for component in suffix.split(std::path::is_separator) {
        match component {
            "" | "." => {}
            ".." => {
                if let Some(parent) = depth.checked_sub(1) {
                    depth = parent;
                } else {
                    let normalized = clean_path(Path::new(path));
                    let root = clean_path(Path::new(source_root));
                    let relative = normalized.strip_prefix(root).ok()?;
                    return Some(std::borrow::Cow::Owned(
                        if relative.as_os_str().is_empty() {
                            String::new()
                        } else {
                            format!("{}{}", std::path::MAIN_SEPARATOR, display_path(relative))
                        },
                    ));
                }
            }
            _ => depth += 1,
        }
    }
    Some(std::borrow::Cow::Borrowed(suffix))
}

fn relocate_rooted_path(path: &str, source_root: &str, target_root: &str) -> Option<String> {
    let suffix = rooted_path_suffix(path, source_root)?;
    Some(format!("{target_root}{suffix}"))
}

fn reference_boundary(ch: char) -> bool {
    ch.is_whitespace() || "\"'`=([{,:;<>)]}|&".contains(ch)
}

fn file_url_starts(raw: &str) -> impl Iterator<Item = usize> + '_ {
    raw.match_indices(':').filter_map(move |(colon, _)| {
        let start = colon.checked_sub(4)?;
        (raw.get(start..colon)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file"))
            && raw[..start]
                .chars()
                .next_back()
                .is_none_or(reference_boundary))
        .then_some(start)
    })
}

fn plain_root_references(
    raw: &str,
    source_root: &str,
    quoted: &[std::ops::Range<usize>],
    file_urls: &[usize],
) -> Vec<usize> {
    let comparison = root_comparison(raw);
    let source = root_comparison(source_root);
    let mut quote_index = 0;
    comparison
        .match_indices(source.as_ref())
        .filter_map(|(start, _)| {
            // In file:/absolute/path, the path belongs to its own URI scheme.
            // It is not a second plain reference after a punctuation boundary.
            if start
                .checked_sub(5)
                .is_some_and(|scheme| file_urls.binary_search(&scheme).is_ok())
            {
                return None;
            }
            let end = start + source_root.len();
            while quoted
                .get(quote_index)
                .is_some_and(|range| range.end <= start)
            {
                quote_index += 1;
            }
            let enclosing = quoted
                .get(quote_index)
                .filter(|range| range.start <= start && range.end >= end);
            let before = raw[..start]
                .chars()
                .next_back()
                .is_none_or(reference_boundary);
            let after = raw[end..]
                .chars()
                .next()
                .is_none_or(std::path::is_separator)
                || enclosing.is_some_and(|range| range.end == end)
                || (enclosing.is_none() && raw[end..].starts_with(['\r', '\n']));
            (before && after).then_some(start)
        })
        .collect()
}

fn rewrite_rooted_text(raw: &str, source_roots: &[String], target_root: &str) -> String {
    // File URLs have their own escaping and authority rules. Preserve other URL
    // components and never interpret an external URL as a local command path.
    let quoted = quoted_value_ranges(raw);
    let mut quote_index = 0;
    let mut urls = Vec::new();
    let mut edits = Vec::new();
    let starts: Vec<_> = file_url_starts(raw).collect();
    let mut plain: Vec<_> = source_roots
        .iter()
        .flat_map(|root| {
            plain_root_references(raw, root, &quoted, &starts)
                .into_iter()
                .map(|start| (start, root.len()))
        })
        .collect();
    plain.sort_unstable_by_key(|(start, len)| (*start, std::cmp::Reverse(*len)));
    plain.dedup_by_key(|(start, _)| *start);
    let mut boundaries = starts.clone();
    boundaries.extend(plain.iter().map(|(start, _)| *start));
    boundaries.sort_unstable();
    boundaries.dedup();
    // A separated opaque escape ends the valid text span; an adjacent escape
    // remains guarded so it cannot create a false root boundary.
    let text_ends: Vec<_> = raw
        .char_indices()
        .filter(|(offset, delimiter)| {
            delimiter.is_control()
                && (*delimiter != '\0'
                    || raw[..*offset]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
        })
        .map(|(offset, _)| offset)
        .chain(std::iter::once(raw.len()))
        .collect();
    let mut text_index = 0;
    let angle_ends: Vec<_> = raw.match_indices('>').map(|(offset, _)| offset).collect();
    for &start in &starts {
        if urls
            .last()
            .is_some_and(|range: &std::ops::Range<usize>| start < range.end)
        {
            continue;
        }
        while quoted
            .get(quote_index)
            .is_some_and(|range| range.end <= start)
        {
            quote_index += 1;
        }
        let enclosing = quoted.get(quote_index).filter(|range| range.start <= start);
        while text_ends[text_index] <= start {
            text_index += 1;
        }
        let quote_end = enclosing
            .map_or(raw.len(), |range| range.end)
            .min(text_ends[text_index]);
        let angle_end = raw[..start]
            .ends_with('<')
            .then(|| {
                angle_ends
                    .get(angle_ends.partition_point(|offset| *offset < start))
                    .copied()
            })
            .flatten()
            .filter(|offset| *offset < quote_end);
        let enclosing_end = angle_end.unwrap_or(quote_end);
        // Every recognized reference starts a new span, irrespective of its
        // representation or separating punctuation. The same plain-path list
        // drives replacement below, so discovery and rewriting cannot disagree.
        let next = boundaries
            .get(boundaries.partition_point(|offset| *offset <= start))
            .copied()
            .filter(|offset| *offset <= enclosing_end);
        let mut value_end = next.unwrap_or(enclosing_end);
        if let Some(next) = next {
            if raw[..next].ends_with('<') {
                value_end -= 1;
            }
            if let Some(space) = raw[start..next].find(char::is_whitespace) {
                if let Some((offset, ch)) = raw[start + space..next]
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| ch.is_whitespace())
                {
                    let word_start = start + space + offset + ch.len_utf8();
                    let prefix = &raw[word_start..next];
                    let label = prefix
                        .strip_suffix('=')
                        .or_else(|| prefix.strip_suffix(':'));
                    if prefix.is_empty()
                        || label.is_some_and(|label| {
                            !label.is_empty()
                                && label
                                    .chars()
                                    .all(|ch| ch.is_alphanumeric() || "_.-".contains(ch))
                        })
                    {
                        value_end = word_start;
                    }
                }
            }
        }
        // This selected span is the complete URI candidate. Never fall back
        // to a whitespace-truncated prefix: ownership can change after spaces.
        // URL syntax ignores trailing ASCII spacing. Preserve it outside the
        // replacement so a bare-root URL gets the same boundary as a child URL.
        value_end = start + raw[start..value_end].trim_end_matches([' ', '\t']).len();
        let range = start..value_end;
        if let Some(relocated) = source_roots
            .iter()
            .find_map(|root| rewrite_file_url(&raw[range.clone()], root, target_root))
        {
            edits.push((range.clone(), relocated));
        }
        urls.push(range);
    }
    // Free-form text has no filesystem or command-language schema. Apply the
    // root boundary rule literally, without interpreting arguments or '..'.
    // Keep the original value intact while locating both URLs and plain paths.
    let mut url_index = 0;
    for (start, len) in plain {
        let end = start + len;
        while urls.get(url_index).is_some_and(|range| range.end <= start) {
            url_index += 1;
        }
        if !urls.get(url_index).is_some_and(|range| range.start < end) {
            edits.push((start..end, target_root.to_string()));
        }
    }
    edits.sort_unstable_by_key(|(range, _)| range.start);
    let mut output = String::with_capacity(raw.len());
    let mut cursor = 0;
    for (range, replacement) in edits {
        if range.start < cursor {
            continue;
        }
        output.push_str(&raw[cursor..range.start]);
        output.push_str(&replacement);
        cursor = range.end;
    }
    output.push_str(&raw[cursor..]);
    output
}

fn decoded_uri_byte(bytes: &[u8], offset: &mut usize) -> Option<u8> {
    let byte = *bytes.get(*offset)?;
    *offset += 1;
    if byte == b'%' {
        if let (Some(high), Some(low)) = (bytes.get(*offset), bytes.get(*offset + 1)) {
            if let (Some(high), Some(low)) = (
                char::from(*high).to_digit(16),
                char::from(*low).to_digit(16),
            ) {
                *offset += 2;
                return Some((high * 16 + low) as u8);
            }
        }
    }
    Some(byte)
}

fn uri_path_prefix_length(raw: &str, prefix: &str) -> Option<usize> {
    let mut offset = 0;
    let mut expected_offset = 0;
    while let Some(expected) = decoded_uri_byte(prefix.as_bytes(), &mut expected_offset) {
        let actual = decoded_uri_byte(raw.as_bytes(), &mut offset)?;
        if actual != expected
            && !(cfg!(windows)
                && std::path::is_separator(char::from(actual))
                && std::path::is_separator(char::from(expected)))
        {
            return None;
        }
    }
    raw.is_char_boundary(offset).then_some(offset)
}

fn rewrite_file_url(value: &str, source_root: &str, target_root: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    let path = url.to_file_path().ok()?;
    let source_url = url::Url::from_file_path(source_root).ok()?;
    let canonical_source = source_url.to_file_path().ok()?;
    let path = path.to_str()?;
    let canonical_source = canonical_source.to_str()?;
    let relocated = relocate_rooted_path(path, canonical_source, target_root)?;
    // URL parsing establishes ownership; splice only the encoded root in the
    // original representation so filenames, query data and surrounding text
    // retain their bytes instead of being serialized again.
    let after_scheme = value.get(5..)?;
    let raw_path = if after_scheme.as_bytes().get(..2).is_some_and(|prefix| {
        prefix
            .iter()
            .all(|byte| std::path::is_separator(char::from(*byte)))
    }) {
        let authority = &after_scheme[2..];
        &authority[authority.find(std::path::is_separator)?..]
    } else {
        after_scheme
    };
    let source_path =
        if !raw_path.starts_with(std::path::is_separator) && source_url.host_str().is_none() {
            source_url.path().strip_prefix('/')?
        } else {
            source_url.path()
        };
    let prefix_length = uri_path_prefix_length(raw_path, source_path)?;
    let suffix = &raw_path[prefix_length..];
    if !suffix.is_empty() && !suffix.starts_with(['?', '#']) {
        let mut offset = 0;
        if !std::path::is_separator(char::from(decoded_uri_byte(
            suffix.as_bytes(),
            &mut offset,
        )?)) {
            return None;
        }
    }
    let destination = url::Url::from_file_path(target_root).ok()?;
    let candidate = format!("{destination}{suffix}");
    if url::Url::parse(&candidate).ok()?.to_file_path().ok()? == Path::new(&relocated) {
        return Some(candidate);
    }
    // A source path can leave and re-enter its root through dot segments. Its
    // original suffix need not have the same meaning below a different root.
    let destination = url::Url::from_file_path(&relocated).ok()?;
    let trailer = value.find(['?', '#']).map_or("", |offset| &value[offset..]);
    Some(format!("{destination}{trailer}"))
}

fn rewrite_path_value(value: &str, source_roots: &[String], target_root: &str) -> String {
    source_roots
        .iter()
        .find_map(|root| {
            relocate_rooted_path(value, root, target_root)
                .or_else(|| rewrite_file_url(value, root, target_root))
        })
        .unwrap_or_else(|| value.to_string())
}

fn json_value_end(raw: &str, start: usize) -> Result<usize, String> {
    let mut values =
        serde_json::Deserializer::from_str(&raw[start..]).into_iter::<serde::de::IgnoredAny>();
    values
        .next()
        .ok_or("missing JSON value")?
        .map_err(|error| error.to_string())?;
    Ok(start + values.byte_offset())
}

fn closing_quote(raw: &str, start: usize, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != '\'' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some(start + offset);
        }
    }
    None
}

fn quoted_value_ranges(raw: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut scan = 0;
    while let Some(offset) = raw[scan..].find(['"', '\'', '`']) {
        let quote_at = scan + offset;
        let quote = raw[quote_at..].chars().next().unwrap();
        let start = quote_at + quote.len_utf8();
        if raw[..quote_at]
            .chars()
            .next_back()
            .is_some_and(|ch| !reference_boundary(ch))
        {
            scan = start;
            continue;
        }
        let end = closing_quote(raw, start, quote).unwrap_or(raw.len());
        ranges.push(start..end);
        scan = end + usize::from(end < raw.len());
    }
    ranges
}

fn with_owner_write_permission<T>(
    path: &Path,
    action: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    let mut writable = permissions.clone();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        writable.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    writable.set_readonly(false);
    if writable == permissions {
        return action();
    }
    fs::set_permissions(path, writable).map_err(|error| error.to_string())?;
    let result = action();
    let restored = fs::set_permissions(path, permissions).map_err(|error| error.to_string());
    match (result, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("failed to restore imported permissions: {error}")),
        (Err(error), Err(restore)) => Err(format!(
            "{error}; failed to restore imported permissions: {restore}"
        )),
    }
}

// Workshop drafts, support files, and rollback receipts share stored hashes.
// OpenClaw owns their validation and target relocation; rewriting even embedded
// path text can invalidate the bundle or change the content restored by rollback.
fn is_integrity_protected_history_path(state_root: &Path, path: &Path) -> bool {
    path.strip_prefix(state_root)
        .is_ok_and(|relative| relative.starts_with("skill-workshop"))
        || is_legacy_audit_history_path(state_root, path)
}

// Use only after containment establishes the actual write destination. For a
// symlink entry replacement, resolve its parent and pass parent/name as the
// destination, without following the old leaf. Keep lexical tree walkers pure.
fn is_integrity_protected_history_destination(
    state_root: &Path,
    resolution_path: &Path,
    destination: &Path,
) -> Result<bool, String> {
    let state_root = match fs::canonicalize(state_root) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let mut entries = super::dev_sources::existing_source_path_entries(resolution_path)?;
    entries.insert(destination.to_path_buf());
    // A protected namespace can itself be an alias into the owned copy. Keep
    // its logical role for both the destination and intermediate file links.
    let namespaces = ["skill-workshop", "logs", "audit"]
        .into_iter()
        .filter_map(|name| {
            let logical = state_root.join(name);
            fs::canonicalize(&logical)
                .ok()
                .map(|resolved| (logical, resolved))
        })
        .collect::<Vec<_>>();
    Ok(entries.iter().any(|entry| {
        let protected = is_integrity_protected_history_path(&state_root, entry)
            || namespaces.iter().any(|(logical, resolved)| {
                entry.strip_prefix(resolved).is_ok_and(|relative| {
                    is_integrity_protected_history_path(&state_root, &logical.join(relative))
                })
            });
        // An alias traversed only to leave it with '..' does not make the
        // resulting ordinary destination part of that history.
        protected
            && (entry == destination
                || fs::canonicalize(entry).is_ok_and(|root| destination.starts_with(root)))
    }))
}

// OpenClaw validates these historical bytes against its audit checkpoints.
// Paths in them are provenance, not runtime references to relocate or repair.
fn is_legacy_audit_history_path(state_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(state_root) else {
        return false;
    };
    let basenames: &[&str] = match relative.parent() {
        Some(parent) if parent == Path::new("logs") => &["config-audit.jsonl"],
        Some(parent) if parent == Path::new("audit") => &["system-agent.jsonl", "crestodian.jsonl"],
        _ => return false,
    };
    let Some(name) = relative.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    basenames.iter().any(|basename| {
        if let Some(claim) = name.strip_prefix('.') {
            return claim
                .strip_prefix(basename)
                .and_then(|suffix| suffix.strip_prefix(".doctor-importing"))
                .is_some_and(is_legacy_audit_generation);
        }
        let Some(suffix) = name.strip_prefix(basename) else {
            return false;
        };
        if suffix.is_empty() {
            return true;
        }
        let Some(archive) = suffix.strip_prefix(".migrated") else {
            return false;
        };
        let (generation, recovery) = archive.split_once(".raw").unwrap_or((archive, ""));
        is_legacy_audit_generation(generation)
            && matches!(
                recovery,
                "" | ".doctor-scrub-restore" | ".doctor-scrub-staging" | ".doctor-scrub-progress"
            )
    })
}

fn is_legacy_audit_generation(suffix: &str) -> bool {
    if suffix.is_empty() {
        return true;
    }
    let Some(generation) = suffix.strip_prefix('.') else {
        return false;
    };
    matches!(generation.as_bytes().first(), Some(b'1'..=b'9'))
        && generation.bytes().all(|byte| byte.is_ascii_digit())
        && (generation.len() > 1 || generation != "1")
}

#[derive(Clone, Copy)]
enum PluginPathRelocation<'a> {
    Imported,
    Cloned {
        source: &'a EnvPaths,
        target: &'a EnvPaths,
    },
}

pub(super) fn relocate_cloned_plugin_index_paths(
    source: &EnvPaths,
    target: &EnvPaths,
) -> Result<MigratedRuntimeStateResult, String> {
    relocate_installed_plugin_index_paths(
        &target.state_dir.join("state/openclaw.sqlite"),
        &source.state_dir,
        &target.state_dir,
        PluginPathRelocation::Cloned { source, target },
    )
}

pub(super) fn clear_cloned_agent_database_leases(target: &EnvPaths) -> Result<(), String> {
    let Some(database_path) = contained_imported_database(
        &target.state_dir.join("state/openclaw.sqlite"),
        &target.root,
        &target.state_dir,
        false,
    )?
    else {
        return Ok(());
    };
    if !is_sqlite_database(&database_path)? {
        return Ok(());
    }
    let connection = Connection::open(&database_path).map_err(|error| error.to_string())?;
    let has_leases = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_database_leases')",
        [],
        |row| row.get::<_, bool>(0),
    ).map_err(|error| error.to_string())?;
    if has_leases {
        // Only a new independent clone loses these source-process claims.
        // Shared repair/import/restore cleanup must preserve its own live owners.
        connection
            .execute("DELETE FROM agent_database_leases", [])
            .map_err(|error| format!("failed to clear cloned agent database leases: {error}"))?;
    }
    Ok(())
}

fn relocate_installed_plugin_index_paths(
    database_path: &Path,
    source_state_root: &Path,
    target_state_root: &Path,
    relocation: PluginPathRelocation<'_>,
) -> Result<MigratedRuntimeStateResult, String> {
    let database_root = match relocation {
        PluginPathRelocation::Imported => target_state_root,
        PluginPathRelocation::Cloned { target, .. } => &target.root,
    };
    let Some(database_path) =
        contained_imported_database(database_path, database_root, target_state_root, false)?
    else {
        return Ok(MigratedRuntimeStateResult::default());
    };
    if !is_sqlite_database(&database_path)? {
        return Ok(MigratedRuntimeStateResult::default());
    }
    let connection = Connection::open(&database_path).map_err(|error| error.to_string())?;
    let table_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'installed_plugin_index'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .is_some();
    let (select_sql, update_sql) = if table_exists {
        (
            "SELECT install_records_json FROM installed_plugin_index WHERE index_key = 'installed-plugin-index'",
            "UPDATE installed_plugin_index SET install_records_json = ?1 WHERE index_key = 'installed-plugin-index'",
        )
    } else {
        let exists = connection.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'config_machine_state'",
            [], |_| Ok(()),
        ).optional().map_err(|error| error.to_string())?.is_some();
        if !exists {
            return Ok(MigratedRuntimeStateResult::default());
        }
        (
            "SELECT value_json FROM config_machine_state WHERE state_key = 'plugins.installedIndex'",
            "UPDATE config_machine_state SET value_json = ?1 WHERE state_key = 'plugins.installedIndex'",
        )
    };
    let raw = connection
        .query_row(select_sql, [], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|error| error.to_string())?;
    let rewritten = raw
        .map(|raw| {
            rewrite_plugin_install_records(
                &raw,
                &database_path,
                table_exists,
                source_state_root,
                target_state_root,
                relocation,
            )
        })
        .transpose()?
        .flatten();
    let changed = rewritten.is_some();
    if let Some(rewritten) = rewritten {
        connection
            .execute(update_sql, [rewritten])
            .map_err(|error| error.to_string())?;
    }
    Ok(MigratedRuntimeStateResult {
        changed,
        ..Default::default()
    })
}

fn rewrite_plugin_install_records(
    raw_records: &str,
    database_path: &Path,
    table_exists: bool,
    source_state_root: &Path,
    target_state_root: &Path,
    relocation: PluginPathRelocation<'_>,
) -> Result<Option<String>, String> {
    let mut document: Value = serde_json::from_str(&raw_records).map_err(|error| {
        format!(
            "failed to parse imported OpenClaw plugin install records {}: {error}",
            display_path(&database_path)
        )
    })?;
    let records = if table_exists {
        document.as_object_mut()
    } else {
        document
            .get_mut("index")
            .and_then(|index| index.get_mut("installRecords"))
            .and_then(Value::as_object_mut)
    };
    let Some(records) = records else {
        return Err(format!(
            "imported OpenClaw plugin install records are not an object: {}",
            display_path(&database_path)
        ));
    };

    let mut changed = false;
    let mut external_plugin_ids = BTreeSet::new();
    for (plugin_id, record) in records.iter_mut() {
        let Some(record) = record.as_object_mut() else {
            continue;
        };
        if let PluginPathRelocation::Cloned { source, target } = relocation {
            match relocate_cloned_plugin_record(record, source, target) {
                Some(record_changed) => changed |= record_changed,
                None => {
                    external_plugin_ids.insert(plugin_id.clone());
                }
            }
            continue;
        }
        let is_path_plugin = record.get("source").and_then(Value::as_str) == Some("path");
        let mut has_external_path = false;
        for field in ["installPath", "sourcePath"] {
            let Some(path) = record.get(field).and_then(Value::as_str) else {
                continue;
            };
            let path = Path::new(path);
            let Some(relative) = relocatable_plugin_path(path, source_state_root, !is_path_plugin)
            else {
                has_external_path |= path.is_absolute() && !path.starts_with(target_state_root);
                continue;
            };
            record.insert(
                field.to_string(),
                Value::String(display_path(&target_state_root.join(&relative))),
            );
            changed = true;
        }
        if has_external_path {
            external_plugin_ids.insert(plugin_id.clone());
        }
    }
    if !external_plugin_ids.is_empty() {
        return Err(format!(
            "imported OpenClaw plugin path(s) could not be isolated inside the env state: {}",
            external_plugin_ids
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if changed {
        Ok(Some(document.to_string()))
    } else {
        Ok(None)
    }
}

fn relocate_cloned_plugin_record(
    record: &mut serde_json::Map<String, Value>,
    source_paths: &EnvPaths,
    target_paths: &EnvPaths,
) -> Option<bool> {
    let path = |field| {
        record
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
    };
    let install_path = path("installPath");
    let source_path = path("sourcePath");
    let is_path_plugin = record.get("source").and_then(Value::as_str) == Some("path");
    let mut changed = false;
    let relocated_install = match install_path.as_deref() {
        Some(path) => Some(cloned_plugin_path(
            path,
            source_paths,
            target_paths,
            !is_path_plugin,
        )?),
        None => None,
    };
    if let Some(install) = relocated_install.as_ref() {
        let value = Value::String(display_path(install));
        changed |= record.get("installPath") != Some(&value);
        record.insert("installPath".to_string(), value);
    }
    if let Some(source) = source_path.as_deref() {
        let relocated_source = if let Some(installed_copy) = relocated_install.as_ref() {
            let install = install_path.as_deref()?;
            let same_payload = install == source
                || matches!((fs::canonicalize(install), fs::canonicalize(source)),
                (Ok(install), Ok(source)) if install == source);
            if same_payload {
                // Preserve the equality used to admit linked source entrypoints.
                installed_copy.clone()
            } else if let Some(copied_source) =
                cloned_plugin_path(source, source_paths, target_paths, !is_path_plugin)
            {
                copied_source
            } else {
                // With an owned preferred installPath, sourcePath is provenance
                // (a project or archive), not a fallback executable location.
                // This also holds while that managed install is missing.
                return Some(changed);
            }
        } else {
            cloned_plugin_path(source, source_paths, target_paths, !is_path_plugin)?
        };
        let value = Value::String(display_path(&relocated_source));
        changed |= record.get("sourcePath") != Some(&value);
        record.insert("sourcePath".to_string(), value);
    }
    Some(changed)
}

fn cloned_plugin_path(
    path: &Path,
    source: &EnvPaths,
    target: &EnvPaths,
    allow_copied_gateway_path: bool,
) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let (relative, target_base, check_source) =
        if let Ok(relative) = path.strip_prefix(&source.root) {
            (relative.to_path_buf(), &target.root, true)
        } else if let Ok(relative) = path.strip_prefix(&target.root) {
            (relative.to_path_buf(), &target.root, false)
        } else if let Some(relative) = existing_plugin_relative(path, &source.root) {
            // Config rewriting also accepts existing aliases of copied env-owned
            // files. Canonicalization here deliberately requires the payload to exist.
            (relative, &target.root, false)
        } else {
            // An old gateway prefix is evidence only when the corresponding copied
            // payload really exists. Missing-path support must not widen this case.
            (
                relocatable_plugin_path(path, &source.state_dir, allow_copied_gateway_path)?,
                &target.state_dir,
                false,
            )
        };
    if relative
        .components()
        .any(|part| !matches!(part, Component::CurDir | Component::Normal(_)))
    {
        return None;
    }
    if check_source && !contained_plugin_path(path, &source.root) {
        return None;
    }
    let copied = target_base.join(relative);
    contained_plugin_path(&copied, &target.root).then_some(copied)
}

fn existing_plugin_relative(path: &Path, root: &Path) -> Option<PathBuf> {
    let root = fs::canonicalize(root).ok()?;
    let path = fs::canonicalize(path).ok()?;
    path.strip_prefix(root).ok().map(Path::to_path_buf)
}

fn contained_plugin_path(path: &Path, root: &Path) -> bool {
    let Ok(root) = fs::canonicalize(root) else {
        return false;
    };
    let Ok((projected, _)) = super::dev_registration::registration_path(path) else {
        return false;
    };
    super::dev_sources::projected_path_contains(&root, &projected).unwrap_or(false)
}

fn contained_imported_database(
    database_path: &Path,
    owned_root: &Path,
    state_root: &Path,
    allow_contained_link: bool,
) -> Result<Option<PathBuf>, String> {
    let metadata = match fs::symlink_metadata(database_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    // Archive copies may retain internal links. Always resolve and check their
    // regular target inside the owned copy before opening SQLite for writing.
    let regular = metadata.is_file()
        || (allow_contained_link
            && metadata.is_symlink()
            && fs::metadata(database_path).is_ok_and(|target| target.is_file()));
    if !regular {
        return Err(format!(
            "imported OpenClaw state database must be a regular file inside the imported state: {}",
            display_path(database_path)
        ));
    }
    let canonical_root = fs::canonicalize(owned_root).map_err(|error| error.to_string())?;
    let canonical_database = fs::canonicalize(database_path).map_err(|error| error.to_string())?;
    if !canonical_database.starts_with(&canonical_root) {
        return Err(format!(
            "imported OpenClaw state database must be a regular file inside the imported state: {}",
            display_path(database_path)
        ));
    }
    // These callers select active copied databases before opening them for
    // writes. Historical bytes cannot also serve as mutable runtime state.
    if is_integrity_protected_history_destination(state_root, database_path, &canonical_database)? {
        return Err(format!(
            "copied OpenClaw database selects integrity-protected history: {}",
            display_path(database_path)
        ));
    }
    Ok(Some(canonical_database))
}

fn relocatable_plugin_path(
    path: &Path,
    source_state_root: &Path,
    allow_copied_gateway_path: bool,
) -> Option<PathBuf> {
    let relative = match path.strip_prefix(source_state_root) {
        Ok(relative) => relative,
        Err(_) if allow_copied_gateway_path => {
            let original_state_root = path
                .ancestors()
                .find(|ancestor| ancestor.file_name() == Some(OsStr::new(".openclaw")))?;
            let relative = path.strip_prefix(original_state_root).ok()?;
            let managed_root = relative.components().next()?.as_os_str();
            if !matches!(
                managed_root.to_str(),
                Some("extensions") | Some("plugins") | Some("npm") | Some("git")
            ) {
                return None;
            }
            relative
        }
        Err(_) => return None,
    };
    if relative
        .components()
        .any(|component| !matches!(component, Component::CurDir | Component::Normal(_)))
    {
        return None;
    }

    let canonical_source = fs::canonicalize(source_state_root).ok()?;
    let canonical_payload = fs::canonicalize(source_state_root.join(relative)).ok()?;
    canonical_payload
        .starts_with(canonical_source)
        .then(|| relative.to_path_buf())
}

// Only runtime-owned locations participate in coherent copies. A SQLite header
// in a package, Workshop bundle, archive, or project fixture is not ownership.
pub(super) struct OpenClawSqliteCopy<'a> {
    state_root: &'a Path,
    registered_agents: BTreeSet<PathBuf>,
}

impl<'a> OpenClawSqliteCopy<'a> {
    pub(super) fn admit_registered_databases(&self, owned_root: &Path) -> Result<(), String> {
        let root = fs::canonicalize(owned_root).map_err(|error| error.to_string())?;
        for selected in &self.registered_agents {
            if fs::canonicalize(selected)
                .is_ok_and(|path| path.starts_with(&root) && path.is_file())
            {
                contained_imported_database(selected, owned_root, self.state_root, true)?;
            }
        }
        Ok(())
    }

    pub(super) fn new(state_root: &'a Path) -> Result<Self, String> {
        let mut selection = Self {
            state_root,
            registered_agents: BTreeSet::new(),
        };
        let database = state_root.join("state/openclaw.sqlite");
        // Preserve the existing handling of linked state; never discover owners
        // by following a database or its parent outside the copied tree.
        if !fs::symlink_metadata(state_root.join("state")).is_ok_and(|meta| meta.is_dir())
            || !fs::symlink_metadata(&database).is_ok_and(|meta| meta.is_file())
            || !is_sqlite_database(&database)?
        {
            return Ok(selection);
        }
        selection.read_agent_registry(&database, false)?;
        Ok(selection)
    }

    fn read_agent_registry(&mut self, database: &Path, archived: bool) -> Result<(), String> {
        let db = if archived {
            // This is a closed snapshot in the private import copy, with no WAL.
            // Read-only WAL connections otherwise create sidecars on discovery.
            let mut location =
                url::Url::from_file_path(database).map_err(|_| "invalid imported database path")?;
            location.set_query(Some("immutable=1"));
            Connection::open_with_flags(
                location.as_str(),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        } else {
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        }
        .map_err(|error| format!("failed to inspect OpenClaw SQLite owners: {error}"))?;
        let has_registry: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_databases')",
            [], |row| row.get(0),
        ).map_err(|error| format!("failed to inspect OpenClaw SQLite owners: {error}"))?;
        if has_registry {
            let mut statement = db
                .prepare("SELECT path FROM agent_databases")
                .map_err(|error| format!("failed to inspect OpenClaw SQLite owners: {error}"))?;
            let paths = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("failed to inspect OpenClaw SQLite owners: {error}"))?;
            for path in paths {
                let path = PathBuf::from(path.map_err(|error| error.to_string())?);
                let path = if path.is_absolute() {
                    path
                } else {
                    self.state_root.join(path)
                };
                // Workspace admission can canonicalize an alias (for example
                // /var on macOS). Match that spelling without expanding the
                // copy footprint or following links during traversal.
                let canonical = fs::canonicalize(&path).or_else(|error| {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        return Err(error);
                    }
                    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                        return Err(error);
                    };
                    fs::canonicalize(parent).map(|parent| parent.join(name))
                });
                if let Ok(canonical) = canonical {
                    if let Ok(root) = fs::canonicalize(self.state_root)
                        && let Ok(relative) = canonical.strip_prefix(root)
                    {
                        self.registered_agents
                            .insert(self.state_root.join(relative));
                    }
                    self.registered_agents.insert(canonical);
                }
                self.registered_agents.insert(path);
            }
        }
        Ok(())
    }

    pub(super) fn contains(&self, path: &Path) -> bool {
        if is_integrity_protected_history_path(self.state_root, path) {
            return false;
        }
        let relative = path.strip_prefix(self.state_root).ok();
        if relative.is_some_and(|path| path.starts_with("imports")) {
            return false;
        }
        if self.registered_agents.contains(path) {
            return true;
        }
        let Some(relative) = relative else {
            return false;
        };
        let components = path_components(relative);
        matches!(
            components.as_slice(),
            ["state", "openclaw.sqlite" | "openclaw-quarantine.sqlite"]
                | ["agents", _, "agent", "openclaw-agent.sqlite"]
                | ["plugins", "workboard", "workboard.sqlite"]
                | ["plugins", "team-reports", "team-reports.sqlite"]
                | ["logbook", "logbook.sqlite"]
        ) || matches!(components.as_slice(), ["memory", name] if name.ends_with(".sqlite"))
            || matches!(components.as_slice(),
                ["matrix", "accounts", _, server_user, token, "state", "openclaw.sqlite" | "openclaw-quarantine.sqlite"]
                if server_user.contains("__") && token.len() == 16 && token.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            )
    }
}

pub(super) fn is_sqlite_database(path: &Path) -> Result<bool, String> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let mut magic = [0_u8; 16];
    Ok(file.read_exact(&mut magic).is_ok() && &magic == b"SQLite format 3\0")
}

fn clear_volatile_runtime_state(
    state_root: &Path,
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<bool, String> {
    if !path_exists(root) {
        return Ok(false);
    }

    let mut changed = false;
    let entries = fs::read_dir(root).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path == config_path
            || workspaces.contains(&path)
            || is_integrity_protected_history_path(state_root, &path)
        {
            continue;
        }

        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() && workspaces.has_descendant(&path) {
            changed |= clear_volatile_runtime_state(state_root, &path, config_path, workspaces)?;
            continue;
        }
        let file_name = entry.file_name();
        if should_remove_volatile_runtime_path(&file_name, metadata.is_dir()) {
            remove_path(&path)?;
            changed = true;
            continue;
        }

        if metadata.is_dir() {
            changed |= clear_volatile_runtime_state(state_root, &path, config_path, workspaces)?;
        }
    }

    Ok(changed)
}

fn clear_parent_preserving_workspaces(
    root: &Path,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<bool, String> {
    if workspaces.contains(root) {
        return Ok(false);
    }

    let mut changed = false;
    let entries = fs::read_dir(root).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if workspaces.contains(&path) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() && workspaces.has_descendant(&path) {
            changed |= clear_parent_preserving_workspaces(&path, workspaces)?;
            continue;
        }
        remove_path(&path)?;
        changed = true;
    }
    Ok(changed)
}

fn prune_agent_runtime_state(
    agents_root: &Path,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<bool, String> {
    if !path_exists(agents_root) {
        return Ok(false);
    }

    let mut changed = false;
    let agent_entries = fs::read_dir(agents_root).map_err(|error| error.to_string())?;
    for entry in agent_entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let agent_root = entry.path();
        if workspaces.contains(&agent_root) {
            continue;
        }
        let metadata = fs::symlink_metadata(&agent_root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() {
            remove_path(&agent_root)?;
            changed = true;
            continue;
        }

        let child_entries = fs::read_dir(&agent_root).map_err(|error| error.to_string())?;
        for child in child_entries {
            let child = child.map_err(|error| error.to_string())?;
            let child_path = child.path();
            if child.file_name() == OsStr::new("agent") || workspaces.contains(&child_path) {
                continue;
            }
            if workspaces.has_descendant(&child_path) {
                changed |= clear_parent_preserving_workspaces(&child_path, workspaces)?;
                continue;
            }
            remove_path(&child_path)?;
            changed = true;
        }

        let mut remaining = fs::read_dir(&agent_root).map_err(|error| error.to_string())?;
        if remaining.next().is_none() {
            remove_path(&agent_root)?;
            changed = true;
        }
    }

    let mut remaining = fs::read_dir(agents_root).map_err(|error| error.to_string())?;
    if remaining.next().is_none() {
        remove_path(agents_root)?;
        changed = true;
    }

    Ok(changed)
}

fn resolve_archivable_workspaces(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<OpenClawWorkspaceInventory, String> {
    let workspaces = resolve_env_openclaw_workspaces(paths, env, runtime)?;
    workspaces.archive_relative_roots(&paths.root)?;
    Ok(workspaces)
}

fn should_remove_volatile_runtime_path(name: &OsStr, is_dir: bool) -> bool {
    if is_dir {
        return matches!(
            name.to_str(),
            Some("run") | Some("tmp") | Some("temp") | Some("locks")
        );
    }

    matches!(
        Path::new(name).extension().and_then(OsStr::to_str),
        Some("pid") | Some("lock") | Some("sock") | Some("socket")
    ) || matches!(
        name.to_str(),
        Some("pid") | Some("lock") | Some("sock") | Some("socket")
    )
}

fn path_components(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect()
}

fn is_durable_openclaw_archive_path(components: &[&str]) -> bool {
    matches!(
        components,
        [".openclaw"]
            | [".openclaw", "workspace", ..]
            | [".openclaw", "openclaw.json"]
            | [".openclaw", "agents", ..]
            | [".openclaw", "credentials", ..]
            | [".openclaw", "cron", ..]
            | [".openclaw", "devices", ..]
            | [".openclaw", "identity", ..]
            | [".openclaw", "memory", ..]
            | [".openclaw", "plugins", ..]
            | [".openclaw", "extensions", ..]
            | [".openclaw", "npm", ..]
            | [".openclaw", "git", ..]
    )
}

fn openclaw_archive_excluded_path_roots(
    paths: &EnvPaths,
    typed: bool,
) -> Result<BTreeSet<PathBuf>, String> {
    Ok(
        openclaw_extension_runtime_debris_roots_with_types(&paths.state_dir, typed)?
            .into_iter()
            .map(|path| Path::new(".openclaw").join(path))
            .collect(),
    )
}

fn openclaw_workspace_archive_excluded_path_roots(
    paths: &EnvPaths,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<BTreeSet<PathBuf>, String> {
    let archive_root = clean_path(&paths.root);
    workspaces
        .workspace_roots()
        .map(|workspace_root| {
            let workspace_root = clean_path(workspace_root);
            let relative_root = workspace_root.strip_prefix(&archive_root).map_err(|_| {
                format!(
                    "configured OpenClaw workspace is outside the environment root: {}",
                    display_path(&workspace_root)
                )
            })?;
            Ok(relative_root.join(".openclaw/tmp"))
        })
        .collect()
}

fn runtime_debris_outside_agent_directories(
    root: &Path,
    paths: &OpenClawWorkspaceInventory,
    candidates: BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    // Inventory admission already rejected internal redirects. Compare owned
    // agent roots and debris candidates using the same root spelling.
    let agent_roots = paths
        .agent_directory_roots()
        .map(canonicalize_path_allow_missing)
        .collect::<Result<Vec<_>, _>>()?;
    if agent_roots.is_empty() {
        return Ok(candidates);
    }
    let canonical_root = canonicalize_path_allow_missing(root)?;
    let mut pending = candidates.into_iter().collect::<Vec<_>>();
    let mut debris = BTreeSet::new();
    while let Some(relative) = pending.pop() {
        let path = canonical_root.join(&relative);
        if agent_roots.iter().any(|root| path.starts_with(root)) {
            continue;
        }
        if agent_roots.iter().any(|root| root.starts_with(&path)) {
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                pending.push(relative.join(entry.file_name()));
            }
        } else {
            debris.insert(relative);
        }
    }
    Ok(debris)
}

fn openclaw_extension_runtime_debris_roots(state_dir: &Path) -> Result<BTreeSet<PathBuf>, String> {
    openclaw_extension_runtime_debris_roots_with_types(state_dir, false)
}

fn openclaw_extension_runtime_debris_roots_with_types(
    state_dir: &Path,
    typed: bool,
) -> Result<BTreeSet<PathBuf>, String> {
    let extensions_root = state_dir.join("extensions");
    let entries = match fs::read_dir(&extensions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut excluded = BTreeSet::new();

    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let plugin_root = entry.path();
        let metadata = fs::symlink_metadata(&plugin_root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() {
            continue;
        }
        let children = fs::read_dir(&plugin_root)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let has_runtime_deps_marker = children.iter().any(|child| {
            is_openclaw_runtime_dependency_marker(&child.file_name())
                && (!typed
                    || child
                        .file_type()
                        .is_ok_and(|kind| runtime_dependency_entry_kind(&child.file_name(), &kind)))
        });
        let plugin_relative_root = Path::new("extensions").join(entry.file_name());

        for child in children {
            let name = child.file_name();
            if is_openclaw_legacy_dependency_debris(&name)
                || (name == OsStr::new("node_modules") && has_runtime_deps_marker)
            {
                if typed {
                    let kind = child.file_type().map_err(|error| error.to_string())?;
                    let expected_kind = if is_openclaw_runtime_dependency_marker(&name) {
                        runtime_dependency_entry_kind(&name, &kind)
                    } else {
                        kind.is_dir()
                    };
                    if !expected_kind {
                        continue;
                    }
                }
                excluded.insert(plugin_relative_root.join(name));
            }
        }
    }

    Ok(excluded)
}

fn runtime_dependency_entry_kind(name: &OsStr, kind: &fs::FileType) -> bool {
    if matches!(
        name.to_str(),
        Some(".openclaw-runtime-deps.json" | ".openclaw-runtime-deps-stamp.json")
    ) {
        kind.is_file()
    } else {
        kind.is_dir()
    }
}

fn is_openclaw_runtime_dependency_marker(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name == ".openclaw-runtime-deps.json"
            || name == ".openclaw-runtime-deps-stamp.json"
            || name.starts_with(".openclaw-runtime-deps-")
    })
}

fn is_openclaw_legacy_dependency_debris(name: &OsStr) -> bool {
    is_openclaw_runtime_dependency_marker(name)
        || name == OsStr::new(".openclaw-pnpm-store")
        || name == OsStr::new(".openclaw-install-backups")
        || name
            .to_str()
            .is_some_and(|name| name.starts_with(".openclaw-install-stage"))
}

fn collect_runtime_state_path_refs(
    paths: &EnvPaths,
    workspaces: &OpenClawWorkspaceInventory,
    current: &EnvMeta,
    known_envs: &[EnvMeta],
    known_refs: &mut BTreeMap<String, (PathBuf, usize)>,
    inferred_refs: &mut BTreeMap<PathBuf, usize>,
) {
    visit_runtime_state_files(
        &paths.state_dir,
        &paths.config_path,
        workspaces,
        &mut |path| {
            if is_integrity_protected_history_path(&paths.state_dir, path) {
                return;
            }
            let Ok(raw) = fs::read_to_string(path) else {
                return;
            };

            for token in candidate_path_tokens(&raw) {
                let candidate = Path::new(token);
                if let Some((env_name, env_root)) =
                    matching_foreign_env_root(current, known_envs, candidate)
                {
                    let entry = known_refs
                        .entry(env_name)
                        .or_insert_with(|| (env_root.clone(), 0));
                    entry.1 += 1;
                    continue;
                }
                if let Some(env_root) = inferred_foreign_env_root(&paths.root, candidate) {
                    *inferred_refs.entry(env_root).or_insert(0) += 1;
                }
            }
        },
    );
}

fn visit_runtime_state_files(
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
    on_file: &mut dyn FnMut(&Path),
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == config_path || workspaces.contains(&path) {
            continue;
        }

        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            visit_runtime_state_files(&path, config_path, workspaces, on_file);
        } else if metadata.is_file() {
            on_file(&path);
        }
    }
}

fn candidate_path_tokens(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
    })
    .filter(|token| !token.is_empty())
}

fn matching_foreign_env_root(
    current: &EnvMeta,
    known_envs: &[EnvMeta],
    path: &Path,
) -> Option<(String, PathBuf)> {
    if !path.is_absolute() {
        return None;
    }

    for env in known_envs {
        if env.name == current.name {
            continue;
        }
        let env_root = clean_path(Path::new(&env.root));
        if path.starts_with(&env_root) {
            return Some((env.name.clone(), env_root));
        }
    }
    None
}

fn inferred_foreign_env_root(current_root: &Path, path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }

    let path = clean_path(path);
    if path.starts_with(current_root) {
        return None;
    }

    let mut prefix = PathBuf::new();
    for component in path.components() {
        if component.as_os_str() == ".openclaw" {
            if prefix.as_os_str().is_empty() {
                return None;
            }
            return Some(clean_path(&prefix));
        }
        prefix.push(component.as_os_str());
    }

    None
}

fn remove_path(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())
    } else {
        fs::remove_file(path).map_err(|error| error.to_string())
    }
}

fn push_issue(issues: &mut Vec<String>, issue: String) {
    if !issues.iter().any(|current| current == &issue) {
        issues.push(issue);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use time::OffsetDateTime;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn protected_history_destination_keeps_alias_roles_and_ordinary_paths() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join(".openclaw");
        let history = temp.path().join("history");
        let ordinary = temp.path().join("ordinary.sqlite");
        let unprotected = temp.path().join("unprotected.sqlite");
        fs::create_dir(&state).unwrap();
        fs::create_dir(&history).unwrap();
        fs::write(&ordinary, b"fixture").unwrap();
        fs::write(&unprotected, b"unprotected fixture").unwrap();
        symlink("../history", state.join("logs")).unwrap();
        symlink("../ordinary.sqlite", history.join("config-audit.jsonl")).unwrap();
        let classify = |path: &Path| {
            is_integrity_protected_history_destination(
                &state,
                path,
                &fs::canonicalize(path).unwrap(),
            )
            .unwrap()
        };
        let selected = state.join("selected.sqlite");
        symlink("logs/config-audit.jsonl", &selected).unwrap();
        assert!(classify(&selected));
        assert!(!classify(&unprotected));

        fs::remove_file(&selected).unwrap();
        fs::create_dir(state.join("skill-workshop")).unwrap();
        symlink("../../history", state.join("skill-workshop/support")).unwrap();
        symlink("skill-workshop/support/config-audit.jsonl", &selected).unwrap();
        assert!(classify(&selected));

        fs::remove_file(&selected).unwrap();
        symlink("skill-workshop/support/../unprotected.sqlite", &selected).unwrap();
        assert!(!classify(&selected));
        assert!(
            !is_integrity_protected_history_destination(
                &temp.path().join("absent"),
                &ordinary,
                &ordinary
            )
            .unwrap()
        );
        let protected_entry = state.join("logs/config-audit.jsonl.migrated");
        symlink("missing", &protected_entry).unwrap();
        let parent = protected_entry.parent().unwrap();
        let destination = fs::canonicalize(parent)
            .unwrap()
            .join(protected_entry.file_name().unwrap());
        assert!(is_integrity_protected_history_destination(&state, parent, &destination).unwrap());
        let ordinary_entry = fs::canonicalize(&state).unwrap().join("ordinary-link");
        symlink("missing", &ordinary_entry).unwrap();
        assert!(
            !is_integrity_protected_history_destination(&state, &state, &ordinary_entry).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn copied_database_relocation_preserves_protected_aliases_in_both_policies() {
        for relocation in [
            DatabasePathRelocation::ArchivedState,
            DatabasePathRelocation::ClonedSessions,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let physical = temp.path().join("physical");
            fs::create_dir(&physical).unwrap();
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&physical, &alias).unwrap();
            let paths = derive_env_paths(alias.join("copy"));
            let source = temp.path().join("source");
            let history = paths
                .state_dir
                .join("skill-workshop/support/history.sqlite");
            let ordinary = paths.state_dir.join("custom/agent.sqlite");
            let entry = serde_json::json!({"spawnedCwd": source.join("workspace")}).to_string();
            for database in [&history, &ordinary] {
                fs::create_dir_all(database.parent().unwrap()).unwrap();
                let db = Connection::open(database).unwrap();
                db.execute_batch("CREATE TABLE session_nodes (entry_json TEXT)")
                    .unwrap();
                db.execute("INSERT INTO session_nodes VALUES (?1)", [&entry])
                    .unwrap();
            }
            let selected = paths.state_dir.join("state/openclaw.sqlite");
            fs::create_dir_all(selected.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink("../skill-workshop/support/history.sqlite", &selected)
                .unwrap();
            let before = fs::read(&history).unwrap();
            let ordinary_before = fs::read(&ordinary).unwrap();
            let error =
                relocate_copied_databases(&paths, &[&source], relocation, None).unwrap_err();
            assert!(
                error.contains("selects integrity-protected history"),
                "{error}"
            );
            assert_eq!(fs::read(&history).unwrap(), before);
            assert_eq!(fs::read(&ordinary).unwrap(), ordinary_before);

            fs::remove_file(&selected).unwrap();
            std::os::unix::fs::symlink("../custom/agent.sqlite", &selected).unwrap();
            let counts = relocate_copied_databases(&paths, &[&source], relocation, None).unwrap();
            assert_eq!(counts[".openclaw/custom/agent.sqlite"], 1);
            assert_eq!(fs::read(&history).unwrap(), before);
            let db = Connection::open(ordinary).unwrap();
            let copied: String = db
                .query_row("SELECT entry_json FROM session_nodes", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                copied,
                serde_json::json!({"spawnedCwd": paths.root.join("workspace")}).to_string()
            );
        }
    }

    fn meta(name: &str, root: &str) -> EnvMeta {
        EnvMeta {
            upgrade_independent_paths: Vec::new(),
            kind: "ocm-env".to_string(),
            name: name.to_string(),
            root: root.to_string(),
            gateway_port: None,
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            dev_ui_port: None,
            protected: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        }
    }

    #[cfg(any(unix, windows))]
    fn fixture_root_spellings(root: &Path) -> Vec<PathBuf> {
        let mut roots = vec![root.to_path_buf(), fs::canonicalize(root).unwrap()];
        if cfg!(windows) {
            roots.push(PathBuf::from(
                roots[1].to_str().unwrap().strip_prefix(r"\\?\").unwrap(),
            ));
            roots.push(PathBuf::from(format!(r"\\?\{}", root.display())));
        }
        roots
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn runtime_state_cleanup_rejects_internal_links_under_root_spellings() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink as symlink_dir;
        #[cfg(windows)]
        use std::os::windows::fs::symlink_dir;
        for copying in [false, true] {
            for spelling in 0..if cfg!(windows) { 4 } else { 2 } {
                let temp = tempfile::tempdir().unwrap();
                let physical = fs::canonicalize(temp.path()).unwrap().join("physical");
                let alias = temp.path().join("alias");
                fs::create_dir_all(&physical).unwrap();
                symlink_dir(&physical, &alias).unwrap();
                let paths = derive_env_paths(alias.join("source"));
                let owned = paths.state_dir.join("owned");
                fs::create_dir_all(&owned).unwrap();
                fs::write(owned.join("notes.txt"), "workspace").unwrap();
                symlink_dir(&owned, paths.state_dir.join("redirect")).unwrap();
                let configured_root = fixture_root_spellings(&paths.root).remove(spelling);
                for agent_directory in [false, true] {
                    let configured = configured_root.join(".openclaw").join("redirect");
                    let agents = if agent_directory {
                        serde_json::json!({"list": [{"id": "main", "agentDir": configured}]})
                    } else {
                        serde_json::json!({"defaults": {"workspace": configured}})
                    };
                    let config = serde_json::json!({"agents": agents}).to_string();
                    fs::write(&paths.config_path, &config).unwrap();
                    fs::write(paths.state_dir.join("gateway.pid"), "123").unwrap();
                    let result = clear_runtime_state_with_policy(
                        &paths,
                        &BTreeMap::new(),
                        OpenClawWorkspaceRuntime::default(),
                        copying,
                    );
                    assert!(
                        result
                            .as_ref()
                            .is_err_and(|error| error.contains("through a symlink")),
                        "copying={copying}, spelling={spelling}, agent_directory={agent_directory}: {result:?}; workspace_exists={}, pid_exists={}",
                        owned.join("notes.txt").exists(),
                        paths.state_dir.join("gateway.pid").exists(),
                    );
                    assert_eq!(fs::read(&paths.config_path).unwrap(), config.as_bytes());
                    assert_eq!(fs::read(owned.join("notes.txt")).unwrap(), b"workspace");
                    assert_eq!(
                        fs::read(paths.state_dir.join("gateway.pid")).unwrap(),
                        b"123"
                    );
                    assert!(paths.state_dir.join("redirect").is_symlink());
                    if !agent_directory {
                        let error = resolve_archivable_workspaces(
                            &paths,
                            &BTreeMap::new(),
                            OpenClawWorkspaceRuntime::default(),
                        )
                        .unwrap_err();
                        assert!(error.contains("through a symlink"), "{error}");
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn file_url_relocation_changes_windows_authority() {
        for (source, target, original, expected) in [
            (
                r"\\old\share\source",
                r"C:\copy",
                "file://old/share/source/plugin.js?q=1#part",
                "file:///C:/copy/plugin.js?q=1#part",
            ),
            (
                r"\\old\share\source",
                r"\\new\share\copy",
                "file://old/share/source/plugin.js",
                "file://new/share/copy/plugin.js",
            ),
            (
                r"C:\source",
                r"\\new\share\copy",
                "file:///C:/source/plugin.js",
                "file://new/share/copy/plugin.js",
            ),
        ] {
            assert_eq!(
                rewrite_file_url(original, source, target).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn audit_detects_foreign_env_refs_in_runtime_state() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-state-audit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let source_root = temp.join("source");
        let target_root = temp.join("target");
        let current = meta("target", &display_path(&target_root));
        let known_envs = vec![meta("source", &display_path(&source_root)), current.clone()];
        fs::create_dir_all(target_root.join(".openclaw/logs")).unwrap();
        fs::write(
            target_root.join(".openclaw/logs/config-audit.jsonl.migrated"),
            format!(
                "{}\n",
                source_root.join(".openclaw/openclaw.json").display()
            ),
        )
        .unwrap();
        let audit = audit_openclaw_state(&current, &known_envs, &BTreeMap::new());
        assert!(!audit.repair_runtime_state);
        assert!(audit.issues.is_empty());

        fs::create_dir_all(target_root.join(".openclaw/agents/main/sessions")).unwrap();
        fs::write(
            target_root.join(".openclaw/agents/main/sessions/main.jsonl"),
            format!(
                "{{\"cwd\":\"{}\"}}\n",
                source_root.join(".openclaw/workspace").display()
            ),
        )
        .unwrap();

        let audit = audit_openclaw_state(&current, &known_envs, &BTreeMap::new());
        assert!(audit.repair_runtime_state);
        assert!(audit.issues.iter().any(|issue| issue.contains(
            "OpenClaw runtime state contains 1 copied path reference(s) under env \"source\" root"
        )));

        let _ = fs::remove_dir_all(&temp);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_state_cleanup_rejects_linked_parents_before_removing_any_entries() {
        for relative in [".openclaw", ".openclaw/agents", ".openclaw/extensions"] {
            let temp = tempfile::tempdir().unwrap();
            let paths = derive_env_paths(temp.path().join("target"));
            fs::create_dir_all(paths.state_dir.join("agents/main/sessions")).unwrap();
            fs::create_dir_all(paths.state_dir.join("extensions/demo/node_modules")).unwrap();
            fs::write(
                paths.state_dir.join("agents/main/sessions/session.jsonl"),
                "session",
            )
            .unwrap();
            fs::write(
                paths
                    .state_dir
                    .join("extensions/demo/node_modules/index.js"),
                "dependency",
            )
            .unwrap();
            fs::write(
                paths
                    .state_dir
                    .join("extensions/demo/.openclaw-runtime-deps.json"),
                "{}",
            )
            .unwrap();
            fs::write(paths.state_dir.join("gateway.pid"), "123").unwrap();
            let linked = paths.root.join(relative);
            let external = temp.path().join("external");
            fs::rename(&linked, &external).unwrap();
            std::os::unix::fs::symlink(&external, &linked).unwrap();

            let error = clear_nonportable_runtime_state(
                &paths,
                &BTreeMap::new(),
                OpenClawWorkspaceRuntime::default(),
            )
            .unwrap_err();
            assert!(error.contains("expected a real directory"), "{error}");
            assert_eq!(
                fs::read_to_string(paths.state_dir.join("agents/main/sessions/session.jsonl"))
                    .unwrap(),
                "session"
            );
            assert_eq!(
                fs::read_to_string(
                    paths
                        .state_dir
                        .join("extensions/demo/node_modules/index.js")
                )
                .unwrap(),
                "dependency"
            );
            assert_eq!(
                fs::read_to_string(paths.state_dir.join("gateway.pid")).unwrap(),
                "123"
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn runtime_cleanup_preserves_agent_retention_policy_under_root_spellings() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink as symlink_dir;
        #[cfg(windows)]
        use std::os::windows::fs::symlink_dir;
        for copying in [false, true] {
            for spelling in 0..if cfg!(windows) { 4 } else { 2 } {
                let temp = tempfile::tempdir().unwrap();
                let physical = temp.path().join("physical");
                let alias = temp.path().join("alias");
                fs::create_dir_all(&physical).unwrap();
                symlink_dir(&physical, &alias).unwrap();
                let paths = derive_env_paths(alias.join("source"));
                let relative = "extensions/demo/node_modules/private-agent";
                let agent = paths.state_dir.join(relative);
                fs::create_dir_all(&agent).unwrap();
                fs::write(agent.join("auth-profiles.json"), "auth fixture").unwrap();
                fs::write(
                    paths
                        .state_dir
                        .join("extensions/demo/node_modules/stale.txt"),
                    "debris",
                )
                .unwrap();
                fs::write(
                    paths
                        .state_dir
                        .join("extensions/demo/.openclaw-runtime-deps.json"),
                    "{}",
                )
                .unwrap();
                let configured = fixture_root_spellings(&agent).remove(spelling);
                let config = serde_json::json!({"agents": {"list": [{"id": "main", "agentDir": configured}]}}).to_string();
                fs::write(&paths.config_path, &config).unwrap();
                assert!(
                    clear_runtime_state_with_policy(
                        &paths,
                        &BTreeMap::new(),
                        OpenClawWorkspaceRuntime::default(),
                        copying,
                    )
                    .unwrap()
                );
                assert_eq!(
                    fs::read(agent.join("auth-profiles.json")).unwrap(),
                    b"auth fixture"
                );
                assert_eq!(fs::read(&paths.config_path).unwrap(), config.as_bytes());
                assert!(
                    !paths
                        .state_dir
                        .join("extensions/demo/node_modules/stale.txt")
                        .exists()
                );
                assert!(
                    !paths
                        .state_dir
                        .join("extensions/demo/.openclaw-runtime-deps.json")
                        .exists()
                );
            }
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn runtime_state_cleanup_rejects_agent_session_collision_under_root_spellings() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink as symlink_dir;
        #[cfg(windows)]
        use std::os::windows::fs::symlink_dir;
        let temp = tempfile::tempdir().unwrap();
        let physical = temp.path().join("physical");
        let alias = temp.path().join("alias");
        fs::create_dir_all(&physical).unwrap();
        symlink_dir(&physical, &alias).unwrap();
        let paths = derive_env_paths(alias.join("target"));
        let sessions = paths.state_dir.join("agents/main/sessions");
        fs::create_dir_all(&sessions).unwrap();
        let payloads = [
            (sessions.join("auth-profiles.json"), "auth fixture"),
            (sessions.join("stale.jsonl"), "stale session"),
            (paths.state_dir.join("gateway.pid"), "123"),
            (
                paths
                    .state_dir
                    .join("extensions/demo/.openclaw-runtime-deps.json"),
                "{}",
            ),
            (
                paths
                    .state_dir
                    .join("extensions/demo/node_modules/index.js"),
                "generated",
            ),
        ];
        for (path, contents) in &payloads {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        for directory in fixture_root_spellings(&sessions) {
            let config = serde_json::json!({"agents":{"entries":{"main":{"agentDir":directory}}}})
                .to_string();
            fs::write(&paths.config_path, &config).unwrap();
            let error = clear_nonportable_runtime_state(
                &paths,
                &BTreeMap::new(),
                OpenClawWorkspaceRuntime::default(),
            )
            .unwrap_err();
            assert!(
                error.contains("configured agent directory overlaps canonical session storage"),
                "{error}"
            );
            assert_eq!(fs::read(&paths.config_path).unwrap(), config.as_bytes());
            for (path, contents) in &payloads {
                assert_eq!(fs::read(path).unwrap(), contents.as_bytes());
            }
        }
    }

    #[test]
    fn clear_nonportable_runtime_state_preserves_config_workspaces_auth_and_plugin_payloads() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-state-clear-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let paths = derive_env_paths(&temp);
        fs::create_dir_all(paths.workspace_dir.join("notes")).unwrap();
        fs::create_dir_all(paths.state_dir.join("workspace-clawforce/skills")).unwrap();
        fs::write(
            &paths.config_path,
            r#"{"agents":{"list":[{"id":"main","default":true},{"id":"clawforce"}]}}"#,
        )
        .unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/agent")).unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/sessions")).unwrap();
        fs::write(
            paths.state_dir.join("agents/main/agent/auth-profiles.json"),
            "{}\n",
        )
        .unwrap();
        for path in [
            "plugins/installs.json",
            "extensions/demo/openclaw.plugin.json",
            "extensions/demo/node_modules/package/index.js",
            "extensions/generated/openclaw.plugin.json",
            "npm/projects/demo/package-lock.json",
            "git/git-demo/repo/.git/HEAD",
        ] {
            let path = paths.state_dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "keep\n").unwrap();
        }
        for path in [
            "extensions/generated/.openclaw-runtime-deps.json",
            "extensions/generated/.openclaw-install-backups/backup.json",
            "extensions/generated/node_modules/package/index.js",
        ] {
            let path = paths.state_dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "remove\n").unwrap();
        }
        fs::write(paths.state_dir.join("logs.txt"), "log\n").unwrap();
        fs::write(paths.workspace_dir.join("notes/todo.txt"), "keep\n").unwrap();
        fs::write(
            paths.state_dir.join("workspace-clawforce/skills/social.md"),
            "keep secondary workspace\n",
        )
        .unwrap();
        fs::create_dir_all(paths.state_dir.join("state")).unwrap();
        fs::write(
            paths.state_dir.join("state/openclaw.sqlite"),
            "paired device state must survive\n",
        )
        .unwrap();

        let changed = clear_nonportable_runtime_state(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(changed);
        assert!(paths.config_path.exists());
        assert!(paths.workspace_dir.join("notes/todo.txt").exists());
        assert!(
            paths
                .state_dir
                .join("workspace-clawforce/skills/social.md")
                .exists()
        );
        assert!(paths.state_dir.join("state/openclaw.sqlite").exists());
        assert!(
            paths
                .state_dir
                .join("agents/main/agent/auth-profiles.json")
                .exists()
        );
        for path in [
            "plugins/installs.json",
            "extensions/demo/openclaw.plugin.json",
            "extensions/demo/node_modules/package/index.js",
            "extensions/generated/openclaw.plugin.json",
            "npm/projects/demo/package-lock.json",
            "git/git-demo/repo/.git/HEAD",
        ] {
            assert!(paths.state_dir.join(path).exists(), "{path}");
        }
        for path in [
            "extensions/generated/.openclaw-runtime-deps.json",
            "extensions/generated/.openclaw-install-backups",
            "extensions/generated/node_modules",
        ] {
            assert!(!paths.state_dir.join(path).exists(), "{path}");
        }
        assert!(!paths.state_dir.join("agents/main/sessions").exists());
        assert!(!paths.state_dir.join("logs.txt").exists());

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn prepare_migrated_runtime_state_preserves_history_and_rewrites_root_refs() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-state-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let source_state_root = temp.join("legacy-home/.openclaw");
        let paths = derive_env_paths(temp.join("target"));
        fs::create_dir_all(paths.workspace_dir.join("notes")).unwrap();
        fs::create_dir_all(paths.state_dir.join("workspace-clawforce/tmp")).unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/agent")).unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/sessions")).unwrap();
        fs::create_dir_all(paths.state_dir.join("logs")).unwrap();
        fs::create_dir_all(paths.state_dir.join("run")).unwrap();
        fs::write(
            &paths.config_path,
            r#"{"agents":{"list":[{"id":"main","default":true},{"id":"clawforce"}]}}"#,
        )
        .unwrap();
        fs::write(paths.workspace_dir.join("notes/todo.txt"), "keep\n").unwrap();
        fs::write(
            paths.state_dir.join("workspace-clawforce/tmp/durable.txt"),
            "keep workspace tmp\n",
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("agents/main/agent/auth-profiles.json"),
            "{}\n",
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("agents/main/sessions/main.jsonl"),
            format!(
                "{{\"cwd\":\"{}\",\"log\":\"{}\"}}\n",
                source_state_root.join("workspace").display(),
                source_state_root.join("logs/gateway.log").display()
            ),
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("logs/gateway.log"),
            format!("source={}\n", source_state_root.join("workspace").display()),
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("openclaw.json.bak"),
            format!("backup={}\n", source_state_root.display()),
        )
        .unwrap();
        fs::write(paths.state_dir.join("gateway.pid"), "4242\n").unwrap();
        fs::write(paths.state_dir.join("run/live.sock"), "sock\n").unwrap();

        let changed = prepare_migrated_runtime_state(
            &paths,
            &source_state_root,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(changed.changed);
        assert!(paths.workspace_dir.join("notes/todo.txt").exists());
        assert!(
            paths
                .state_dir
                .join("workspace-clawforce/tmp/durable.txt")
                .exists()
        );
        assert!(
            paths
                .state_dir
                .join("agents/main/sessions/main.jsonl")
                .exists()
        );
        assert!(paths.state_dir.join("logs/gateway.log").exists());
        assert!(paths.state_dir.join("openclaw.json.bak").exists());
        assert!(!paths.state_dir.join("gateway.pid").exists());
        assert!(!paths.state_dir.join("run").exists());

        let session_raw =
            fs::read_to_string(paths.state_dir.join("agents/main/sessions/main.jsonl")).unwrap();
        let logs_raw = fs::read_to_string(paths.state_dir.join("logs/gateway.log")).unwrap();
        let backup_raw = fs::read_to_string(paths.state_dir.join("openclaw.json.bak")).unwrap();
        assert!(session_raw.contains(&display_path(&paths.state_dir.join("workspace"))));
        assert!(session_raw.contains(&display_path(&paths.state_dir.join("logs/gateway.log"))));
        assert!(!session_raw.contains(&display_path(&source_state_root)));
        assert!(logs_raw.contains(&display_path(&paths.state_dir.join("workspace"))));
        assert!(!logs_raw.contains(&display_path(&source_state_root)));
        assert!(backup_raw.contains(&display_path(&paths.state_dir)));
        assert!(!backup_raw.contains(&display_path(&source_state_root)));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn archive_policy_omits_openclaw_tmp_inside_configured_workspaces() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-workspace-tmp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let archive_path = temp.with_extension("tar");
        let extract_dir = temp.with_extension("extracted");
        let _ = fs::remove_file(&archive_path);
        let _ = fs::remove_dir_all(&extract_dir);
        let paths = derive_env_paths(&temp);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            format!(
                r#"{{"agents":{{"defaults":{{"workspace":"{}"}}}}}}"#,
                paths.state_dir.join("team").display()
            ),
        )
        .unwrap();

        for path in [
            "workspace/notes.txt",
            "workspace/tmp/keep.txt",
            "workspace/.openclaw/tmp/cache.bin",
            "workspace/.openclaw/tmp-keep/data.bin",
            "team/notes.txt",
            "team/.openclaw/tmp/cache.bin",
        ] {
            let path = paths.state_dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "fixture\n").unwrap();
        }

        let options = openclaw_env_snapshot_archive_options(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        crate::infra::archive::write_env_archive_with_options(
            &serde_json::json!({ "kind": "test" }),
            &paths.root,
            &archive_path,
            options,
        )
        .unwrap();
        let extracted = crate::infra::archive::extract_env_archive::<serde_json::Value>(
            &archive_path,
            &extract_dir,
        )
        .unwrap();

        for path in [
            "workspace/notes.txt",
            "workspace/tmp/keep.txt",
            "workspace/.openclaw/tmp-keep/data.bin",
            "team/notes.txt",
        ] {
            assert!(
                extracted.root_dir.join(".openclaw").join(path).exists(),
                "{path}"
            );
        }
        for path in [
            "workspace/.openclaw/tmp/cache.bin",
            "team/.openclaw/tmp/cache.bin",
        ] {
            assert!(
                !extracted.root_dir.join(".openclaw").join(path).exists(),
                "{path}"
            );
        }

        let _ = fs::remove_dir_all(&temp);
        let _ = fs::remove_file(&archive_path);
        let _ = fs::remove_dir_all(&extract_dir);
    }

    #[test]
    fn archive_policy_uses_configured_workspace_roots_without_prefix_guessing() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-archive-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let paths = derive_env_paths(&temp);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            format!(
                r#"{{"agents":{{"defaults":{{"workspace":"{}"}},"list":[{{"id":"primary","default":true}},{{"id":"ops"}}]}}}}"#,
                paths.state_dir.join("team").display()
            ),
        )
        .unwrap();

        let export_options = openclaw_env_archive_options(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        let options = openclaw_env_snapshot_archive_options(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(
            options
                .included_path_roots
                .contains(Path::new(".openclaw/team"))
        );
        assert!(
            options
                .included_path_roots
                .contains(Path::new(".openclaw/workspace"))
        );
        assert!(
            options
                .included_path_roots
                .contains(Path::new(".openclaw/state"))
        );
        for options in [&export_options, &options] {
            assert!(
                options
                    .excluded_path_roots
                    .contains(Path::new(".openclaw/team/.openclaw/tmp"))
            );
            assert!(
                options
                    .excluded_path_roots
                    .contains(Path::new(".openclaw/workspace/.openclaw/tmp"))
            );
        }
        assert!(options.snapshot_sqlite_files);
        assert!(export_options.snapshot_sqlite_files);
        assert!(
            !export_options
                .included_path_roots
                .contains(Path::new(".openclaw/state"))
        );
        assert!(should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/state/openclaw.sqlite"),
            EnvArchiveEntryKind::File
        ));
        assert!(!should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/workspace/notes.txt"),
            EnvArchiveEntryKind::File
        ));
        for path in [
            ".openclaw/plugins/installs.json",
            ".openclaw/extensions/demo/package.json",
            ".openclaw/npm/projects/demo/package-lock.json",
            ".openclaw/npm/projects/demo/node_modules/demo/index.js",
            ".openclaw/git/git-demo/repo/.git/HEAD",
            ".openclaw/git/git-demo/repo/openclaw.plugin.json",
        ] {
            assert!(
                !should_skip_openclaw_env_archive_path(Path::new(path), EnvArchiveEntryKind::File),
                "{path} should be archived"
            );
        }
        for path in [
            ".openclaw/plugin-runtime-deps/node_modules/demo/index.js",
            ".openclaw/bundled-plugin-runtime-deps/node_modules/demo/index.js",
            ".openclaw/.openclaw-pnpm-store/v3/files/cache",
            ".openclaw/.openclaw-npm-cache/_cacache/index",
        ] {
            assert!(
                should_skip_openclaw_env_archive_path(Path::new(path), EnvArchiveEntryKind::File),
                "{path} should not be archived"
            );
        }
        assert!(should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/workspace-attestations/manifest.json"),
            EnvArchiveEntryKind::File
        ));
        assert!(should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/workspace-cache/data.json"),
            EnvArchiveEntryKind::File
        ));

        let _ = fs::remove_dir_all(&temp);
    }
}
