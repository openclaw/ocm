mod binding;
mod execution;
mod health;
mod inspect;
mod lifecycle;
mod snapshots;
mod source_watch;
mod source_watch_session;

use std::collections::BTreeMap;
use std::path::Path;

pub use execution::ExecutionSummary;
pub use execution::{
    ExecutionBinding, GatewayProcessSpec, ResolvedExecution, resolve_execution_binding,
    resolve_gateway_process_spec, resolve_runtime_run_dir,
};
pub use health::{
    EnvCleanupActionSummary, EnvCleanupBatchSummary, EnvCleanupSummary, EnvDoctorSummary,
};
pub use inspect::EnvStatusSummary;
pub use lifecycle::{
    CloneEnvironmentOptions, CreateEnvironmentOptions, EnvDevMeta, EnvExportSummary,
    EnvImportSummary, EnvMeta, EnvSummary, ExportEnvironmentOptions, ImportEnvironmentOptions,
    default_service_enabled, default_service_running, select_prune_candidates,
};
pub(crate) use lifecycle::{CloneEnvironmentResult, ImportEnvironmentResult};
pub use snapshots::{
    CreateEnvSnapshotOptions, EnvSnapshotRemoveSummary, EnvSnapshotRestoreSummary,
    EnvSnapshotSummary, RemoveEnvSnapshotOptions, RestoreEnvSnapshotOptions,
    UpgradeCheckpointScope, select_snapshot_prune_candidates,
};
pub(crate) use source_watch::{
    CreateSourceWatchOverrideOptions, SourceWatchLease, SourceWatchMode, SourceWatchState,
};
pub use source_watch::{SourceWatchEndpoint, SourceWatchOverride};
pub(crate) use source_watch_session::{SourceWatchCompletion, SourceWatchSession};

pub struct EnvironmentService<'a> {
    env: &'a BTreeMap<String, String>,
    cwd: &'a Path,
}

impl<'a> EnvironmentService<'a> {
    pub fn new(env: &'a BTreeMap<String, String>, cwd: &'a Path) -> Self {
        Self { env, cwd }
    }
}
