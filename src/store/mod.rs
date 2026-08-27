mod checkpoints;
mod common;
mod envs;
mod gateway_ports;
mod launchers;
mod layout;
mod openclaw_config;
mod openclaw_state;
mod openclaw_workspaces;
mod runtimes;
mod snapshots;
mod upgrade_history;

use std::collections::BTreeMap;
use std::path::Path;

use time::OffsetDateTime;

use crate::env::EnvMeta;
use crate::env::EnvSummary;
pub(crate) use common::{
    ExclusiveFileLock, copy_dir_recursive, ensure_dir, lock_file, read_json, write_json,
};
pub(crate) use envs::{EnvironmentOperationLock, lock_env_registry, lock_environment_operation};
pub(crate) use envs::{
    EnvironmentServicePolicyChange, restore_environment_service_policy,
    set_environment_service_policy,
};
pub use envs::{
    clone_environment, create_environment, export_environment, get_environment, import_environment,
    list_environments, remove_environment, save_environment,
};
pub(crate) use envs::{
    clone_environment_for_simulation, clone_environment_with_sandbox_origin,
    create_environment_with_validated_runtime, import_environment_with_sandbox_origin,
};
pub(crate) use envs::{
    save_environment_with_validated_launcher, save_environment_with_validated_runtime,
    with_locked_environments,
};
pub(crate) use gateway_ports::{
    openclaw_port_family_available, openclaw_port_family_range, resolve_config_gateway_port,
    resolve_effective_gateway_ports, resolve_env_gateway_port,
};
pub use launchers::{add_launcher, get_launcher, list_launchers, remove_launcher};
pub use layout::{
    EnvPaths, StorePaths, clean_path, default_env_root, derive_env_paths, display_path,
    env_registry_path, launcher_meta_path, resolve_absolute_path, resolve_ocm_home,
    resolve_store_paths, resolve_user_home, runtime_install_files_dir, runtime_install_root,
    runtime_meta_path, snapshot_archive_path, snapshot_checkpoint_path, snapshot_env_dir,
    snapshot_meta_path, source_watch_override_path, supervisor_logs_dir, supervisor_runtime_path,
    supervisor_state_path, upgrade_history_env_dir, upgrade_history_meta_path,
    upgrade_history_recovery_dir, upgrade_history_runtime_recovery_dir, validate_name,
};
pub(crate) use openclaw_config::{
    OpenClawConfigAudit, audit_openclaw_config, clear_skip_bootstrap_for_openclaw_onboarding,
    ensure_minimum_local_openclaw_config, normalize_new_environment_sandbox_origin,
    openclaw_config_include_paths, openclaw_config_uses_includes,
    reject_include_owned_agent_workspaces, reject_include_owned_sandbox_origin,
    repair_openclaw_config, rewrite_external_workspace_paths_for_migration,
    rewrite_identity_bound_workspace_paths_for_target, rewrite_openclaw_config_for_migration,
    rewrite_openclaw_config_for_new_environment, rewrite_openclaw_config_for_simulation,
    rewrite_openclaw_config_for_target, rewrite_openclaw_config_includes_for_target,
};
pub(crate) use openclaw_state::{
    OpenClawStateAudit, audit_openclaw_state, clear_nonportable_runtime_state,
    openclaw_env_archive_options, prepare_migrated_runtime_state, repair_openclaw_runtime_state,
};
pub(crate) use openclaw_workspaces::{
    OpenClawWorkspaceRuntime, resolve_env_openclaw_workspaces, resolve_plain_openclaw_workspaces,
};
pub(crate) use runtimes::install_runtime_from_local_openclaw_build;
pub(crate) use runtimes::install_runtime_from_selected_official_openclaw_release;
pub(crate) use runtimes::{
    BuildLocalRuntimeOptions, InstallContext, PreparedRuntimeInstall, RuntimeReleaseDetails,
    prepare_runtime_from_selected_official_openclaw_release, prepare_runtime_from_selected_release,
    runtime_operational_issue,
};
pub use runtimes::{
    add_runtime, get_runtime, get_runtime_verified, install_runtime,
    install_runtime_from_official_openclaw_release, install_runtime_from_release,
    install_runtime_from_url, list_runtimes, remove_runtime, runtime_integrity_issue,
};
pub(crate) use snapshots::{
    EnvSnapshotRestoreTransaction, PreparedEnvSnapshotCapture, commit_env_snapshot_restore,
    create_env_snapshot_from_preparation, prepare_env_snapshot_capture,
    prepare_env_snapshot_restore, rollback_env_snapshot_restore,
};
pub use snapshots::{
    create_env_snapshot, get_env_snapshot, list_all_env_snapshots, list_env_snapshots,
    remove_env_snapshot, restore_env_snapshot, summarize_snapshot,
};
pub use upgrade_history::{
    UpgradeHistoryBinding, UpgradeHistoryPhaseTiming, UpgradeHistoryRecord,
    UpgradeHistoryRuntimeRecovery, UpgradeHistoryServiceState, UpgradeHistoryStage,
    get_upgrade_history_record, list_upgrade_history, save_upgrade_history_record,
};
pub(crate) use upgrade_history::{
    UpgradeRuntimeRecovery, get_upgrade_runtime_recovery, lock_upgrade_transaction,
    remove_upgrade_recovery, remove_upgrade_recovery_for_snapshot,
};

pub fn now_utc() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

pub fn ensure_store(env: &BTreeMap<String, String>, cwd: &Path) -> Result<StorePaths, String> {
    let stores = resolve_store_paths(env, cwd)?;
    ensure_dir(&stores.home)?;
    ensure_dir(&stores.envs_dir)?;
    ensure_dir(&stores.launchers_dir)?;
    ensure_dir(&stores.runtimes_dir)?;
    ensure_dir(&stores.snapshots_dir)?;
    ensure_dir(&stores.upgrade_history_dir)?;
    ensure_dir(&stores.supervisor_dir)?;
    Ok(stores)
}

pub fn summarize_env(meta: &EnvMeta) -> EnvSummary {
    let paths = derive_env_paths(Path::new(&meta.root));
    EnvSummary {
        name: meta.name.clone(),
        root: display_path(&paths.root),
        openclaw_home: display_path(&paths.openclaw_home),
        state_dir: display_path(&paths.state_dir),
        config_path: display_path(&paths.config_path),
        workspace_dir: display_path(&paths.workspace_dir),
        gateway_port: meta.gateway_port,
        service_enabled: meta.service_enabled,
        service_running: meta.service_running,
        default_runtime: meta.default_runtime.clone(),
        default_launcher: meta.default_launcher.clone(),
        dev_repo_root: meta.dev.as_ref().map(|dev| dev.repo_root.clone()),
        dev_worktree_root: meta.dev.as_ref().map(|dev| dev.worktree_root.clone()),
        protected: meta.protected,
        created_at: meta.created_at,
        last_used_at: meta.last_used_at,
    }
}
