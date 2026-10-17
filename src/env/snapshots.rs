use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use super::{EnvMeta, EnvironmentService};
use crate::store::{
    EnvSnapshotRestoreTransaction, PreparedEnvSnapshotCapture, SnapshotRestoreError,
    commit_env_snapshot_restore, create_env_snapshot, create_env_snapshot_from_preparation,
    ensure_restore_preserves_dev_sources, get_env_snapshot, list_all_env_snapshots,
    list_env_snapshots, now_utc, prepare_env_snapshot_capture, prepare_env_snapshot_restore,
    prepare_upgrade_checkpoint_capture, prepare_upgrade_snapshot_restore, remove_env_snapshot,
    rollback_env_snapshot_restore, summarize_snapshot,
};
use crate::supervisor::sync_supervisor_env_if_present;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeCheckpointScope {
    pub independent_paths: Vec<std::path::PathBuf>,
}

#[derive(Clone, Debug)]
pub struct CreateEnvSnapshotOptions {
    pub env_name: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvSnapshotSummary {
    pub id: String,
    pub env_name: String,
    pub label: Option<String>,
    pub archive_path: String,
    pub storage_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_scope: Option<UpgradeCheckpointScope>,
    pub source_root: String,
    pub gateway_port: Option<u32>,
    pub service_enabled: bool,
    pub service_running: bool,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub protected: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvSnapshotRestoreSummary {
    pub env_name: String,
    pub snapshot_id: String,
    pub label: Option<String>,
    pub root: String,
    pub archive_path: String,
    pub storage_kind: String,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub protected: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvSnapshotRemoveSummary {
    pub env_name: String,
    pub snapshot_id: String,
    pub label: Option<String>,
    pub archive_path: String,
    pub storage_kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RestoreEnvSnapshotOptions {
    pub env_name: String,
    pub snapshot_id: String,
}

#[derive(Clone, Debug)]
pub struct RemoveEnvSnapshotOptions {
    pub env_name: String,
    pub snapshot_id: String,
}

pub fn select_snapshot_prune_candidates(
    snapshots: &[EnvSnapshotSummary],
    keep: Option<usize>,
    older_than_days: Option<i64>,
    now: OffsetDateTime,
) -> Vec<EnvSnapshotSummary> {
    let mut grouped = BTreeMap::<String, Vec<EnvSnapshotSummary>>::new();
    for snapshot in snapshots {
        grouped
            .entry(snapshot.env_name.clone())
            .or_default()
            .push(snapshot.clone());
    }

    let cutoff = older_than_days.map(|days| now - Duration::days(days));
    let keep = keep.unwrap_or(0);
    let mut out = Vec::new();

    for snapshots in grouped.values_mut() {
        sort_snapshots(snapshots);
        for (index, snapshot) in snapshots.iter().enumerate() {
            if index < keep {
                continue;
            }
            if let Some(cutoff) = cutoff
                && snapshot.created_at > cutoff
            {
                continue;
            }
            out.push(snapshot.clone());
        }
    }

    sort_snapshots(&mut out);
    out
}

fn sort_snapshots(snapshots: &mut [EnvSnapshotSummary]) {
    snapshots.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
}

impl<'a> EnvironmentService<'a> {
    pub fn create_snapshot(
        &self,
        options: CreateEnvSnapshotOptions,
    ) -> Result<EnvSnapshotSummary, String> {
        let _lock = self.lock_operation(&options.env_name)?;
        self.create_snapshot_locked(options)
    }

    pub(crate) fn create_snapshot_locked(
        &self,
        options: CreateEnvSnapshotOptions,
    ) -> Result<EnvSnapshotSummary, String> {
        let meta = create_env_snapshot(options, self.env, self.cwd)?;
        Ok(summarize_snapshot(&meta))
    }

    pub(crate) fn prepare_snapshot_capture_locked(
        &self,
        env_name: &str,
    ) -> Result<PreparedEnvSnapshotCapture, String> {
        prepare_env_snapshot_capture(env_name, self.env, self.cwd)
    }

    pub(crate) fn prepare_upgrade_checkpoint_locked(
        &self,
        env_name: &str,
    ) -> Result<PreparedEnvSnapshotCapture, String> {
        prepare_upgrade_checkpoint_capture(env_name, self.env, self.cwd)
    }

    pub(crate) fn create_snapshot_locked_from_preparation(
        &self,
        options: CreateEnvSnapshotOptions,
        service_state: Option<(bool, bool)>,
        prepared: PreparedEnvSnapshotCapture,
    ) -> Result<EnvSnapshotSummary, String> {
        let meta = create_env_snapshot_from_preparation(
            options,
            service_state,
            prepared,
            self.env,
            self.cwd,
        )?;
        Ok(summarize_snapshot(&meta))
    }

    pub fn list_snapshots(
        &self,
        env_name: Option<&str>,
    ) -> Result<Vec<EnvSnapshotSummary>, String> {
        let snapshots = match env_name {
            Some(env_name) => list_env_snapshots(env_name, self.env, self.cwd)?,
            None => list_all_env_snapshots(self.env, self.cwd)?,
        };
        Ok(snapshots.iter().map(summarize_snapshot).collect())
    }

    pub fn get_snapshot(
        &self,
        env_name: &str,
        snapshot_id: &str,
    ) -> Result<EnvSnapshotSummary, String> {
        let snapshot = get_env_snapshot(env_name, snapshot_id, self.env, self.cwd)?;
        Ok(summarize_snapshot(&snapshot))
    }

    pub(crate) fn ensure_snapshot_restore_preserves_dev_sources_locked(
        &self,
        snapshot: &EnvSnapshotSummary,
    ) -> Result<(), String> {
        ensure_restore_preserves_dev_sources(
            &self.get(&snapshot.env_name)?,
            snapshot
                .upgrade_scope
                .as_ref()
                .map(|scope| scope.independent_paths.as_slice())
                .unwrap_or_default(),
            self.env,
            self.cwd,
        )
    }

    pub(crate) fn ensure_upgrade_rollback_preserves_dev_sources_locked(
        &self,
        meta: &EnvMeta,
    ) -> Result<(), String> {
        ensure_restore_preserves_dev_sources(
            meta,
            &meta.upgrade_independent_paths,
            self.env,
            self.cwd,
        )
    }

    pub fn restore_snapshot(
        &self,
        options: RestoreEnvSnapshotOptions,
    ) -> Result<EnvSnapshotRestoreSummary, String> {
        let _lock = self.lock_operation(&options.env_name)?;
        self.restore_snapshot_locked(options)
    }

    pub(crate) fn restore_snapshot_locked(
        &self,
        options: RestoreEnvSnapshotOptions,
    ) -> Result<EnvSnapshotRestoreSummary, String> {
        let transaction = self
            .prepare_snapshot_restore_locked(options)
            .map_err(|error| error.to_string())?;
        Ok(self.commit_snapshot_restore_locked(transaction))
    }

    pub(crate) fn prepare_snapshot_restore_locked(
        &self,
        options: RestoreEnvSnapshotOptions,
    ) -> Result<EnvSnapshotRestoreTransaction, SnapshotRestoreError> {
        let transaction = prepare_env_snapshot_restore(options, self.env, self.cwd)?;
        self.sync_snapshot_restore_locked(transaction, None)
    }

    pub(crate) fn prepare_upgrade_snapshot_restore_locked(
        &self,
        options: RestoreEnvSnapshotOptions,
        runtime_guard: Option<&crate::store::RuntimeMutationGuard>,
    ) -> Result<EnvSnapshotRestoreTransaction, String> {
        let transaction =
            prepare_upgrade_snapshot_restore(options, runtime_guard, self.env, self.cwd)
                .map_err(|error| error.to_string())?;
        self.sync_snapshot_restore_locked(transaction, runtime_guard)
            .map_err(|error| error.to_string())
    }

    fn sync_snapshot_restore_locked(
        &self,
        transaction: EnvSnapshotRestoreTransaction,
        runtime_guard: Option<&crate::store::RuntimeMutationGuard>,
    ) -> Result<EnvSnapshotRestoreTransaction, SnapshotRestoreError> {
        let env_name = &transaction.summary.env_name;
        if let Err(sync_error) = sync_supervisor_env_if_present(self.env, self.cwd, env_name) {
            let error = format!("snapshot restore failed supervisor synchronization: {sync_error}");
            return Err(
                match self.rollback_snapshot_restore_with_guard_locked(transaction, runtime_guard) {
                    Ok(()) => SnapshotRestoreError {
                        message: format!(
                            "{error}; pre-restore environment root and registry were restored"
                        ),
                        prior_state_restored: true,
                    },
                    Err(rollback_error) => SnapshotRestoreError {
                        message: format!("{error}; {rollback_error}"),
                        prior_state_restored: rollback_error.prior_state_restored,
                    },
                },
            );
        }
        Ok(transaction)
    }

    pub(crate) fn commit_snapshot_restore_locked(
        &self,
        transaction: EnvSnapshotRestoreTransaction,
    ) -> EnvSnapshotRestoreSummary {
        commit_env_snapshot_restore(transaction)
    }

    pub(crate) fn rollback_snapshot_restore_locked(
        &self,
        transaction: EnvSnapshotRestoreTransaction,
    ) -> Result<(), SnapshotRestoreError> {
        self.rollback_snapshot_restore_with_guard_locked(transaction, None)
    }

    fn rollback_snapshot_restore_with_guard_locked(
        &self,
        transaction: EnvSnapshotRestoreTransaction,
        runtime_guard: Option<&crate::store::RuntimeMutationGuard>,
    ) -> Result<(), SnapshotRestoreError> {
        let env_name = transaction.summary.env_name.clone();
        let cleanup_warning =
            rollback_env_snapshot_restore(transaction, runtime_guard, self.env, self.cwd).map_err(
                |message| SnapshotRestoreError {
                    message,
                    prior_state_restored: false,
                },
            )?;
        let sync_result = sync_supervisor_env_if_present(self.env, self.cwd, &env_name).map(|_| ());
        let result = match (cleanup_warning, sync_result) {
            (None, Ok(())) => Ok(()),
            (Some(cleanup_warning), Ok(())) => Err(cleanup_warning),
            (None, Err(sync_error)) => Err(format!("supervisor resync failed: {sync_error}")),
            (Some(cleanup_warning), Err(sync_error)) => Err(format!(
                "{cleanup_warning}; supervisor resync also failed: {sync_error}"
            )),
        };
        result.map_err(|error| SnapshotRestoreError {
            message: format!("pre-restore environment root and registry were restored; recovery requires attention: {error}"),
            prior_state_restored: true,
        })
    }

    pub fn remove_snapshot(
        &self,
        options: RemoveEnvSnapshotOptions,
    ) -> Result<EnvSnapshotRemoveSummary, String> {
        let _lock = self.lock_operation(&options.env_name)?;
        self.remove_snapshot_locked(options)
    }

    pub(crate) fn remove_snapshot_locked(
        &self,
        options: RemoveEnvSnapshotOptions,
    ) -> Result<EnvSnapshotRemoveSummary, String> {
        remove_env_snapshot(options, self.env, self.cwd)
    }

    pub fn prune_snapshot_candidates(
        &self,
        env_name: Option<&str>,
        keep: Option<usize>,
        older_than_days: Option<i64>,
    ) -> Result<Vec<EnvSnapshotSummary>, String> {
        let snapshots = self.list_snapshots(env_name)?;
        Ok(select_snapshot_prune_candidates(
            &snapshots,
            keep,
            older_than_days,
            now_utc(),
        ))
    }

    pub fn prune_snapshots(
        &self,
        env_name: Option<&str>,
        keep: Option<usize>,
        older_than_days: Option<i64>,
    ) -> Result<Vec<EnvSnapshotRemoveSummary>, String> {
        let candidates = self.prune_snapshot_candidates(env_name, keep, older_than_days)?;
        let mut removed = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let _lock = self.lock_operation(&candidate.env_name)?;
            removed.push(remove_env_snapshot(
                RemoveEnvSnapshotOptions {
                    env_name: candidate.env_name,
                    snapshot_id: candidate.id,
                },
                self.env,
                self.cwd,
            )?);
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::{EnvSnapshotSummary, select_snapshot_prune_candidates};
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    fn restore_fixture(
        scoped: bool,
    ) -> (
        tempfile::TempDir,
        std::collections::BTreeMap<String, String>,
        super::RestoreEnvSnapshotOptions,
    ) {
        let root = tempfile::tempdir().unwrap();
        let env = std::collections::BTreeMap::from([
            (
                "OCM_HOME".into(),
                root.path().join("ocm-home").to_string_lossy().into_owned(),
            ),
            (
                "HOME".into(),
                root.path().join("home").to_string_lossy().into_owned(),
            ),
        ]);
        let meta = crate::store::create_environment(
            crate::env::CreateEnvironmentOptions {
                name: "source".into(),
                root: None,
                gateway_port: Some(19789),
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev: None,
                protected: false,
            },
            &env,
            root.path(),
        )
        .unwrap();
        let marker = std::path::Path::new(&meta.root).join("marker");
        std::fs::write(&marker, "snapshot").unwrap();
        let preparation = if scoped {
            let mut meta = meta.clone();
            meta.upgrade_independent_paths = vec!["project".into()];
            crate::store::save_environment(meta.clone(), &env, root.path()).unwrap();
            std::fs::create_dir_all(std::path::Path::new(&meta.root).join("project")).unwrap();
            crate::store::prepare_upgrade_checkpoint_capture("source", &env, root.path()).unwrap()
        } else {
            crate::store::prepare_env_snapshot_capture("source", &env, root.path()).unwrap()
        };
        let snapshot = crate::store::create_env_snapshot_from_preparation(
            super::CreateEnvSnapshotOptions {
                env_name: "source".into(),
                label: None,
            },
            None,
            preparation,
            &env,
            root.path(),
        )
        .unwrap();
        std::fs::write(&marker, "current").unwrap();
        if scoped {
            std::fs::write(
                std::path::Path::new(&meta.root).join("project/independent"),
                "keep current",
            )
            .unwrap();
        }
        (
            root,
            env,
            super::RestoreEnvSnapshotOptions {
                env_name: "source".into(),
                snapshot_id: snapshot.id,
            },
        )
    }

    #[test]
    fn upgrade_restore_sync_failure_rolls_back_with_the_held_runtime_guard() {
        for scoped in [false, true] {
            let (root, env, options) = restore_fixture(scoped);
            let service = super::EnvironmentService::new(&env, root.path());
            let mut original = service.get("source").unwrap();
            original.default_runtime = Some("current".into());
            crate::store::save_environment(original.clone(), &env, root.path()).unwrap();
            let guard = crate::store::lock_runtime_mutation("current", &env, root.path()).unwrap();
            // A real filesystem write failure, without changing permissions or service state.
            let state = crate::store::supervisor_state_path(&env, root.path()).unwrap();
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(state.join("obstruction"), "keep").unwrap();
            let error = service
                .prepare_upgrade_snapshot_restore_locked(options, Some(&guard))
                .unwrap_err();
            assert!(
                error.contains("pre-restore environment root and registry were restored"),
                "{error}"
            );
            assert_eq!(
                service.get("source").unwrap().default_runtime,
                original.default_runtime
            );
            assert_eq!(
                std::fs::read_to_string(std::path::Path::new(&original.root).join("marker"))
                    .unwrap(),
                "current"
            );
            assert!(
                !std::fs::read_dir(root.path().join("ocm-home/envs"))
                    .unwrap()
                    .filter_map(Result::ok)
                    .any(|entry| entry.file_name().to_string_lossy().contains("ocm-restore"))
            );
            if scoped {
                assert_eq!(
                    std::fs::read_to_string(
                        root.path().join("ocm-home/envs/source/project/independent")
                    )
                    .unwrap(),
                    "keep current"
                );
            }
        }
    }

    #[test]
    fn restore_rollback_failure_reports_retained_custody_and_actual_root() {
        for scoped in [false, true] {
            let (root, env, options) = restore_fixture(scoped);
            let service = super::EnvironmentService::new(&env, root.path());
            let transaction =
                crate::store::prepare_env_snapshot_restore(options, &env, root.path()).unwrap();
            let operation = std::fs::read_dir(root.path().join("ocm-home/envs"))
                .unwrap()
                .filter_map(Result::ok)
                .find(|entry| entry.file_name().to_string_lossy().contains("ocm-restore"))
                .unwrap()
                .path();
            let registry = crate::store::env_registry_path(&env, root.path()).unwrap();
            std::fs::remove_file(&registry).unwrap();
            std::fs::create_dir(&registry).unwrap();
            let error = service
                .rollback_snapshot_restore_locked(transaction)
                .unwrap_err();
            assert!(!error.prior_state_restored);
            let error = error.message;
            assert!(
            error.contains(
                if scoped { "failed to restore the environment registry entry; the restored owned entries were reinstated" } else { "failed to restore the environment registry entry; the restored root was reinstated" }
            ),
            "{error}"
        );
            assert!(
                error.contains(&format!(
                    "restore operation retained at {}",
                    operation.display()
                )),
                "{error}"
            );
            assert_eq!(
                std::fs::read_to_string(root.path().join("ocm-home/envs/source/marker")).unwrap(),
                "snapshot"
            );
            assert_eq!(
                std::fs::read_to_string(operation.join("displaced/marker")).unwrap(),
                "current"
            );
            if scoped {
                assert_eq!(
                    std::fs::read_to_string(
                        root.path().join("ocm-home/envs/source/project/independent")
                    )
                    .unwrap(),
                    "keep current"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn restored_data_is_reported_separately_from_rejected_tree_cleanup_failure() {
        let (root, env, options) = restore_fixture(false);
        let service = super::EnvironmentService::new(&env, root.path());
        let transaction =
            crate::store::prepare_env_snapshot_restore(options, &env, root.path()).unwrap();
        let marker = root.path().join("ocm-home/envs/source/marker");
        assert!(
            std::process::Command::new("/usr/bin/chflags")
                .arg("uchg")
                .arg(&marker)
                .status()
                .unwrap()
                .success()
        );
        let error = service
            .rollback_snapshot_restore_locked(transaction)
            .unwrap_err();
        assert!(error.prior_state_restored);
        let error = error.message;
        let operation = std::fs::read_dir(root.path().join("ocm-home/envs"))
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().contains("ocm-restore"))
            .unwrap()
            .path();
        let rejected_marker = operation.join("rejected/marker");
        assert!(
            std::process::Command::new("/usr/bin/chflags")
                .arg("nouchg")
                .arg(&rejected_marker)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            error.contains("pre-restore environment root and registry were restored"),
            "{error}"
        );
        assert!(
            error.contains(&operation.to_string_lossy().to_string()),
            "{error}"
        );
        assert!(error.contains("could not be removed"), "{error}");
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "current");
        assert_eq!(
            std::fs::read_to_string(rejected_marker).unwrap(),
            "snapshot"
        );
    }

    #[test]
    fn snapshot_prune_selection_keeps_the_newest_snapshots_per_environment() {
        let now = parse_time("2026-03-25T22:00:00Z");
        let snapshots = vec![
            snapshot("alpha-3", "alpha", now - time::Duration::days(1)),
            snapshot("alpha-2", "alpha", now - time::Duration::days(2)),
            snapshot("alpha-1", "alpha", now - time::Duration::days(3)),
            snapshot("beta-2", "beta", now - time::Duration::days(1)),
            snapshot("beta-1", "beta", now - time::Duration::days(2)),
        ];

        let candidates = select_snapshot_prune_candidates(&snapshots, Some(1), None, now);
        let ids = candidates
            .iter()
            .map(|snapshot| snapshot.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["beta-1", "alpha-2", "alpha-1"]);
    }

    #[test]
    fn snapshot_prune_selection_respects_age_cutoffs_after_the_keep_floor() {
        let now = parse_time("2026-03-25T22:00:00Z");
        let snapshots = vec![
            snapshot("alpha-new", "alpha", now - time::Duration::days(1)),
            snapshot("alpha-old", "alpha", now - time::Duration::days(10)),
            snapshot("beta-kept", "beta", now - time::Duration::days(30)),
            snapshot("beta-old", "beta", now - time::Duration::days(40)),
        ];

        let candidates = select_snapshot_prune_candidates(&snapshots, Some(1), Some(7), now);
        let ids = candidates
            .iter()
            .map(|snapshot| snapshot.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["alpha-old", "beta-old"]);
    }

    fn snapshot(id: &str, env_name: &str, created_at: OffsetDateTime) -> EnvSnapshotSummary {
        EnvSnapshotSummary {
            upgrade_scope: None,
            id: id.to_string(),
            env_name: env_name.to_string(),
            label: None,
            archive_path: format!("/tmp/{id}.tar"),
            storage_kind: "tar-archive-v1".to_string(),
            source_root: format!("/tmp/{env_name}"),
            gateway_port: None,
            service_enabled: false,
            service_running: false,
            default_runtime: None,
            default_launcher: None,
            protected: false,
            created_at,
        }
    }

    fn parse_time(raw: &str) -> OffsetDateTime {
        OffsetDateTime::parse(raw, &Rfc3339).unwrap()
    }
}
