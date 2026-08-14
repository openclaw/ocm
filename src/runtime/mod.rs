mod install;
mod launch;
mod registry;
pub mod releases;
mod verify;

pub(crate) use install::StagedRuntimeInstall;
pub use install::{
    BuildLocalRuntimeOptions, InstallRuntimeFromOfficialReleaseOptions,
    InstallRuntimeFromReleaseOptions, InstallRuntimeFromUrlOptions, InstallRuntimeOptions,
    OfficialRuntimePrepareAction, RuntimeUpdateBatchSummary, RuntimeUpdateSummary,
    UpdateRuntimeFromReleaseOptions,
};
pub(crate) use launch::{
    is_official_openclaw_package_runtime, is_openclaw_package_runtime, resolve_runtime_launch,
};
pub use registry::{
    AddRuntimeOptions, RuntimeMeta, RuntimeReleaseSelectorKind, RuntimeService, RuntimeSourceKind,
};
pub use releases::{
    OpenClawRelease, OpenClawReleaseCatalogEntry, RuntimeRelease, RuntimeReleaseManifest,
};
pub use verify::{RuntimeBinarySummary, RuntimeVerifySummary};
