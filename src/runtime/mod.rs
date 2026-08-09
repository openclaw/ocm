mod install;
mod launch;
mod npm_proxy;
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
pub(crate) use npm_proxy::{
    INTERNAL_NPM_PROXY_REAL_BIN_ENV, INTERNAL_NPM_PROXY_WORKSPACE_DIRS_ENV,
    INTERNAL_NPM_PROXY_WORKSPACE_VERSIONS_ENV,
};
#[doc(hidden)]
pub use npm_proxy::{is_internal_npm_proxy, run_internal_npm_proxy};
pub use registry::{
    AddRuntimeOptions, RuntimeCompanionMeta, RuntimeMeta, RuntimeReleaseSelectorKind,
    RuntimeService, RuntimeSourceKind,
};
pub use releases::{
    OpenClawRelease, OpenClawReleaseCatalogEntry, RuntimeRelease, RuntimeReleaseManifest,
};
pub use verify::{RuntimeBinarySummary, RuntimeVerifySummary};
