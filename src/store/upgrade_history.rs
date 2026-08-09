use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::common::{
    ExclusiveFileLock, load_json_files, lock_file, path_exists, read_json, write_json,
};
use super::layout::{
    display_path, runtime_install_root, upgrade_history_env_dir, upgrade_history_meta_path,
    upgrade_history_recovery_dir, validate_name,
};
use super::runtime_integrity_issue;
use crate::runtime::{RuntimeMeta, RuntimeSourceKind};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeHistoryBinding {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub openclaw_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeHistoryServiceState {
    pub enabled: bool,
    pub running: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeHistoryStage {
    pub status: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeHistoryRuntimeRecovery {
    pub runtime_name: String,
    #[serde(default)]
    pub release_version: Option<String>,
    #[serde(default)]
    pub backup_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeHistoryPhaseTiming {
    pub owner: String,
    pub phase: String,
    pub window: String,
    pub started_offset_ms: u64,
    pub duration_ms: u64,
    pub outcome: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeHistoryRecord {
    pub kind: String,
    pub format_version: u32,
    pub id: String,
    pub env_name: String,
    pub source: UpgradeHistoryBinding,
    pub target: UpgradeHistoryBinding,
    pub snapshot_id: String,
    #[serde(default)]
    pub runtime_recovery: Vec<UpgradeHistoryRuntimeRecovery>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    pub outcome: String,
    pub migration: UpgradeHistoryStage,
    pub finalization: UpgradeHistoryStage,
    #[serde(default)]
    pub phases: Vec<UpgradeHistoryPhaseTiming>,
    pub service_before: UpgradeHistoryServiceState,
    pub service_after: UpgradeHistoryServiceState,
    #[serde(default)]
    pub rollback: Option<String>,
    #[serde(default)]
    pub rollback_of: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct UpgradeRuntimeRecovery {
    pub meta: RuntimeMeta,
    pub install_root: PathBuf,
}

pub fn save_upgrade_history_record(
    record: &UpgradeHistoryRecord,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let env_name = validate_name(&record.env_name, "Environment name")?;
    let transaction_id = validate_name(&record.id, "Upgrade transaction id")?;
    validate_upgrade_history_record(record)?;
    let path = upgrade_history_meta_path(&env_name, &transaction_id, env, cwd)?;
    write_json(&path, record)
}

pub fn get_upgrade_history_record(
    env_name: &str,
    transaction_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<UpgradeHistoryRecord, String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let transaction_id = validate_name(transaction_id, "Upgrade transaction id")?;
    let path = upgrade_history_meta_path(&env_name, &transaction_id, env, cwd)?;
    if !path_exists(&path) {
        return Err(format!(
            "upgrade transaction \"{transaction_id}\" does not exist for environment \"{env_name}\""
        ));
    }
    let record = read_json(&path)?;
    validate_upgrade_history_record(&record)?;
    validate_upgrade_history_identity(&record, &env_name, &transaction_id)?;
    Ok(record)
}

pub fn list_upgrade_history(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<UpgradeHistoryRecord>, String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let dir = upgrade_history_env_dir(&env_name, env, cwd)?;
    let files = load_json_files(&dir)?;
    let mut records: Vec<UpgradeHistoryRecord> = Vec::with_capacity(files.len());
    for file in files {
        let record = read_json(&file)?;
        validate_upgrade_history_record(&record)?;
        let transaction_id = file
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                format!(
                    "upgrade history path has an invalid transaction id: {}",
                    display_path(&file)
                )
            })?;
        validate_upgrade_history_identity(&record, &env_name, transaction_id)?;
        records.push(record);
    }
    records.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(records)
}

pub(crate) fn lock_upgrade_transaction(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ExclusiveFileLock, String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let path = super::ensure_store(env, cwd)?
        .home
        .join("locks")
        .join("upgrades")
        .join(format!("{env_name}.lock"));
    lock_file(&path, "upgrade transaction")
}

pub(crate) fn get_upgrade_runtime_recovery(
    env_name: &str,
    transaction_id: &str,
    snapshot_id: &str,
    runtime_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<UpgradeRuntimeRecovery, String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let transaction_id = validate_name(transaction_id, "Upgrade transaction id")?;
    let runtime_name = validate_name(runtime_name, "Runtime name")?;
    let transaction_recovery_root =
        upgrade_history_recovery_dir(&env_name, &transaction_id, env, cwd)?;
    let recovery_snapshot_path = transaction_recovery_root.join("snapshot-id");
    let recovery_snapshot_id =
        std::fs::read_to_string(&recovery_snapshot_path).map_err(|error| {
            format!(
                "runtime recovery snapshot marker is unavailable at {}: {error}",
                display_path(&recovery_snapshot_path)
            )
        })?;
    if recovery_snapshot_id.trim() != snapshot_id {
        return Err(format!(
            "runtime recovery for upgrade transaction \"{transaction_id}\" belongs to snapshot \"{}\", expected \"{snapshot_id}\"",
            recovery_snapshot_id.trim()
        ));
    }
    let recovery_root = transaction_recovery_root.join(&runtime_name);
    let install_root = recovery_root.join("install-root");
    let meta_path = recovery_root.join("runtime.json");
    if !path_exists(&meta_path) || !path_exists(&install_root) {
        return Err(format!(
            "runtime recovery for \"{runtime_name}\" is unavailable for upgrade transaction \"{transaction_id}\""
        ));
    }

    let meta: RuntimeMeta = read_json(&meta_path)?;
    if meta.name != runtime_name {
        return Err(format!(
            "runtime recovery metadata at {} belongs to \"{}\", expected \"{runtime_name}\"",
            display_path(&meta_path),
            meta.name
        ));
    }
    let expected_install_root = runtime_install_root(&runtime_name, env, cwd)?;
    if meta.source_kind != RuntimeSourceKind::Installed
        || meta
            .install_root
            .as_deref()
            .map(Path::new)
            .is_none_or(|path| path != expected_install_root)
    {
        return Err(format!(
            "runtime recovery metadata for \"{runtime_name}\" is not an installer-managed runtime"
        ));
    }
    let original_binary = Path::new(&meta.binary_path);
    let relative_binary = original_binary
        .strip_prefix(&expected_install_root)
        .map_err(|_| {
            format!(
                "runtime recovery metadata for \"{runtime_name}\" points outside its managed install root"
            )
        })?;
    let recovery_binary = install_root.join(relative_binary);
    let mut relocated = meta.clone();
    relocated.binary_path = display_path(&recovery_binary);
    relocated.install_root = Some(display_path(&install_root));
    if let Some(issue) = runtime_integrity_issue(&relocated, env) {
        return Err(format!(
            "runtime recovery for \"{runtime_name}\" is not healthy: {issue}"
        ));
    }

    Ok(UpgradeRuntimeRecovery { meta, install_root })
}

pub(crate) fn remove_upgrade_recovery(
    env_name: &str,
    transaction_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let transaction_id = validate_name(transaction_id, "Upgrade transaction id")?;
    let path = upgrade_history_recovery_dir(&env_name, &transaction_id, env, cwd)?;
    if !path_exists(&path) {
        return Ok(());
    }
    std::fs::remove_dir_all(&path).map_err(|error| {
        format!(
            "failed to remove upgrade recovery at {}: {error}",
            display_path(&path)
        )
    })
}

pub(crate) fn remove_upgrade_recovery_for_snapshot(
    env_name: &str,
    snapshot_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let recovery_parent = upgrade_history_env_dir(&env_name, env, cwd)?;
    if !recovery_parent.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&recovery_parent).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let path = entry.path();
        if !file_type.is_dir() || !entry.file_name().to_string_lossy().ends_with(".recovery") {
            continue;
        }
        let Ok(recovery_snapshot_id) = std::fs::read_to_string(path.join("snapshot-id")) else {
            continue;
        };
        if recovery_snapshot_id.trim() == snapshot_id {
            std::fs::remove_dir_all(&path).map_err(|error| {
                format!(
                    "failed to remove upgrade recovery at {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn validate_upgrade_history_record(record: &UpgradeHistoryRecord) -> Result<(), String> {
    if record.kind != "ocm-upgrade-transaction" {
        return Err(format!("unsupported upgrade history kind: {}", record.kind));
    }
    if record.format_version != 1 {
        return Err(format!(
            "unsupported upgrade history format version: {}",
            record.format_version
        ));
    }
    for phase in &record.phases {
        if !matches!(phase.owner.as_str(), "ocm" | "openclaw") {
            return Err(format!("unsupported upgrade timing owner: {}", phase.owner));
        }
        if !matches!(
            phase.window.as_str(),
            "preparation" | "stopped" | "post-ready"
        ) {
            return Err(format!(
                "unsupported upgrade timing window: {}",
                phase.window
            ));
        }
        if !matches!(
            phase.outcome.as_str(),
            "completed" | "failed" | "warning" | "skipped" | "deferred" | "already-observed"
        ) {
            return Err(format!(
                "unsupported upgrade timing outcome: {}",
                phase.outcome
            ));
        }
        if phase.phase.is_empty()
            || !phase
                .phase
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("upgrade timing phase must be a non-empty identifier".to_string());
        }
    }
    Ok(())
}

fn validate_upgrade_history_identity(
    record: &UpgradeHistoryRecord,
    env_name: &str,
    transaction_id: &str,
) -> Result<(), String> {
    if record.env_name != env_name {
        return Err(format!(
            "upgrade transaction \"{transaction_id}\" belongs to environment \"{}\", expected \"{env_name}\"",
            record.env_name
        ));
    }
    if record.id != transaction_id {
        return Err(format!(
            "upgrade history entry \"{transaction_id}\" contains transaction id \"{}\"",
            record.id
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use time::OffsetDateTime;

    use super::{
        RuntimeMeta, RuntimeSourceKind, UpgradeHistoryBinding, UpgradeHistoryPhaseTiming,
        UpgradeHistoryRecord, UpgradeHistoryRuntimeRecovery, UpgradeHistoryServiceState,
        UpgradeHistoryStage, get_upgrade_history_record, get_upgrade_runtime_recovery,
        list_upgrade_history, lock_upgrade_transaction, runtime_install_root,
        save_upgrade_history_record, write_json,
    };
    use crate::store::layout::upgrade_history_runtime_recovery_dir;

    fn test_env(label: &str) -> (PathBuf, BTreeMap<String, String>) {
        let root = std::env::temp_dir().join(format!(
            "ocm-upgrade-history-{label}-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_HOME".to_string(),
            root.join("ocm").to_string_lossy().into_owned(),
        );
        (root, env)
    }

    fn record(id: &str, started_at: OffsetDateTime) -> UpgradeHistoryRecord {
        UpgradeHistoryRecord {
            kind: "ocm-upgrade-transaction".to_string(),
            format_version: 1,
            id: id.to_string(),
            env_name: "demo".to_string(),
            source: UpgradeHistoryBinding {
                kind: "runtime".to_string(),
                name: "stable".to_string(),
                openclaw_version: Some("2026.6.11".to_string()),
            },
            target: UpgradeHistoryBinding {
                kind: "runtime".to_string(),
                name: "2026.6.33".to_string(),
                openclaw_version: Some("2026.6.33".to_string()),
            },
            snapshot_id: "snapshot-1".to_string(),
            runtime_recovery: vec![UpgradeHistoryRuntimeRecovery {
                runtime_name: "stable".to_string(),
                release_version: Some("2026.6.11".to_string()),
                backup_id: None,
            }],
            started_at,
            completed_at: started_at,
            outcome: "switched".to_string(),
            migration: UpgradeHistoryStage {
                status: "completed".to_string(),
                note: None,
            },
            finalization: UpgradeHistoryStage {
                status: "completed".to_string(),
                note: None,
            },
            phases: vec![UpgradeHistoryPhaseTiming {
                owner: "ocm".to_string(),
                phase: "preparation".to_string(),
                window: "preparation".to_string(),
                started_offset_ms: 0,
                duration_ms: 10,
                outcome: "completed".to_string(),
            }],
            service_before: UpgradeHistoryServiceState {
                enabled: true,
                running: true,
            },
            service_after: UpgradeHistoryServiceState {
                enabled: true,
                running: true,
            },
            rollback: None,
            rollback_of: None,
            note: None,
        }
    }

    #[test]
    fn upgrade_history_round_trips_and_sorts_newest_first() {
        let (root, env) = test_env("round-trip");
        let cwd = root.as_path();
        let older = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let newer = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();

        save_upgrade_history_record(&record("1700000000-000000001", older), &env, cwd).unwrap();
        save_upgrade_history_record(&record("1800000000-000000001", newer), &env, cwd).unwrap();

        let records = list_upgrade_history("demo", &env, cwd).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            vec!["1800000000-000000001", "1700000000-000000001"]
        );
        let loaded = get_upgrade_history_record("demo", "1700000000-000000001", &env, cwd).unwrap();
        assert_eq!(loaded.source.openclaw_version.as_deref(), Some("2026.6.11"));
        assert_eq!(loaded.target.openclaw_version.as_deref(), Some("2026.6.33"));
        assert_eq!(loaded.phases, record("ignored", older).phases);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upgrade_history_reads_records_without_rollback_linkage() {
        let (root, env) = test_env("legacy-rollback-linkage");
        let cwd = root.as_path();
        let record = serde_json::json!({
            "kind": "ocm-upgrade-transaction",
            "formatVersion": 1,
            "id": "1700000000-000000001",
            "envName": "demo",
            "source": {
                "kind": "runtime",
                "name": "stable",
                "openclawVersion": "2026.6.11"
            },
            "target": {
                "kind": "runtime",
                "name": "stable",
                "openclawVersion": "2026.6.33"
            },
            "snapshotId": "snapshot-1",
            "runtimeRecovery": [],
            "startedAt": "2023-11-14T22:13:20Z",
            "completedAt": "2023-11-14T22:13:20Z",
            "outcome": "updated",
            "migration": {"status": "validated"},
            "finalization": {"status": "completed"},
            "serviceBefore": {"enabled": true, "running": true},
            "serviceAfter": {"enabled": true, "running": true}
        });
        let path =
            super::upgrade_history_meta_path("demo", "1700000000-000000001", &env, cwd).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

        let loaded = get_upgrade_history_record("demo", "1700000000-000000001", &env, cwd).unwrap();
        assert!(loaded.rollback_of.is_none());
        assert!(loaded.phases.is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upgrade_history_rejects_unstructured_phase_labels() {
        let (root, env) = test_env("phase-label");
        let started_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let mut record = record("1700000000-000000001", started_at);
        record.phases[0].phase = "config=/private/token".to_string();

        let error = save_upgrade_history_record(&record, &env, root.as_path()).unwrap_err();
        assert_eq!(error, "upgrade timing phase must be a non-empty identifier");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upgrade_history_rejects_records_with_mismatched_identity() {
        let (root, env) = test_env("mismatched-identity");
        let cwd = root.as_path();
        let transaction_id = "1700000000-000000001";
        let path = super::upgrade_history_meta_path("demo", transaction_id, &env, cwd).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let started_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let mut mismatched = record(transaction_id, started_at);
        mismatched.env_name = "other".to_string();
        write_json(&path, &mismatched).unwrap();

        let error = get_upgrade_history_record("demo", transaction_id, &env, cwd).unwrap_err();
        assert!(
            error.contains("belongs to environment \"other\", expected \"demo\""),
            "{error}"
        );
        let error = list_upgrade_history("demo", &env, cwd).unwrap_err();
        assert!(
            error.contains("belongs to environment \"other\", expected \"demo\""),
            "{error}"
        );

        mismatched.env_name = "demo".to_string();
        mismatched.id = "1700000000-000000002".to_string();
        write_json(&path, &mismatched).unwrap();
        let error = get_upgrade_history_record("demo", transaction_id, &env, cwd).unwrap_err();
        assert!(
            error.contains(
                "upgrade history entry \"1700000000-000000001\" contains transaction id \"1700000000-000000002\""
            ),
            "{error}"
        );
        let error = list_upgrade_history("demo", &env, cwd).unwrap_err();
        assert!(
            error.contains(
                "upgrade history entry \"1700000000-000000001\" contains transaction id \"1700000000-000000002\""
            ),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_recovery_loads_only_healthy_managed_bytes() {
        let (root, env) = test_env("runtime-recovery");
        let cwd = root.as_path();
        let runtime_name = "stable";
        let transaction_id = "1700000000-000000001";
        let expected_install_root = runtime_install_root(runtime_name, &env, cwd).unwrap();
        let relative_binary = PathBuf::from("files/bin/openclaw");
        let recovery_root =
            upgrade_history_runtime_recovery_dir("demo", transaction_id, runtime_name, &env, cwd)
                .unwrap();
        let recovery_install_root = recovery_root.join("install-root");
        std::fs::create_dir_all(recovery_install_root.join("files/bin")).unwrap();
        std::fs::write(recovery_install_root.join(&relative_binary), b"openclaw").unwrap();
        let created_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let meta = RuntimeMeta {
            kind: "ocm-runtime".to_string(),
            name: runtime_name.to_string(),
            binary_path: expected_install_root
                .join(&relative_binary)
                .to_string_lossy()
                .into_owned(),
            source_kind: RuntimeSourceKind::Installed,
            source_path: None,
            source_url: Some("https://example.invalid/openclaw.tgz".to_string()),
            source_manifest_url: Some("https://example.invalid/openclaw".to_string()),
            source_sha256: None,
            source_integrity: None,
            runtime_sha256: None,
            release_version: Some("2026.6.11".to_string()),
            release_channel: Some("stable".to_string()),
            release_selector_kind: None,
            release_selector_value: None,
            install_root: Some(expected_install_root.to_string_lossy().into_owned()),
            companions: Vec::new(),
            description: None,
            created_at,
            updated_at: created_at,
        };
        write_json(&recovery_root.join("runtime.json"), &meta).unwrap();
        std::fs::write(
            recovery_root.parent().unwrap().join("snapshot-id"),
            "snapshot-1",
        )
        .unwrap();

        let recovery = get_upgrade_runtime_recovery(
            "demo",
            transaction_id,
            "snapshot-1",
            runtime_name,
            &env,
            cwd,
        )
        .unwrap();
        assert_eq!(recovery.meta.release_version.as_deref(), Some("2026.6.11"));
        assert_eq!(recovery.install_root, recovery_install_root);

        std::fs::write(
            recovery_root.parent().unwrap().join("snapshot-id"),
            "snapshot-2",
        )
        .unwrap();
        let error = get_upgrade_runtime_recovery(
            "demo",
            transaction_id,
            "snapshot-1",
            runtime_name,
            &env,
            cwd,
        )
        .unwrap_err();
        assert!(
            error.contains("belongs to snapshot \"snapshot-2\", expected \"snapshot-1\""),
            "{error}"
        );
        std::fs::write(
            recovery_root.parent().unwrap().join("snapshot-id"),
            "snapshot-1",
        )
        .unwrap();

        let mut invalid = meta;
        invalid.install_root = Some(root.join("foreign").to_string_lossy().into_owned());
        write_json(&recovery_root.join("runtime.json"), &invalid).unwrap();
        let error = get_upgrade_runtime_recovery(
            "demo",
            transaction_id,
            "snapshot-1",
            runtime_name,
            &env,
            cwd,
        )
        .unwrap_err();
        assert!(
            error.contains("not an installer-managed runtime"),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upgrade_transaction_lock_serializes_the_same_environment() {
        let (root, env) = test_env("transaction-lock");
        let cwd = root.as_path();
        let first = lock_upgrade_transaction("demo", &env, cwd).unwrap();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let thread_env = env.clone();
        let thread_cwd = root.clone();
        let waiter = thread::spawn(move || {
            let _second =
                lock_upgrade_transaction("demo", &thread_env, thread_cwd.as_path()).unwrap();
            acquired_tx.send(()).unwrap();
        });

        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        drop(first);
        acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();

        let _ = std::fs::remove_dir_all(root);
    }
}
