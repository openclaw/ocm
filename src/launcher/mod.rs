mod registry;
mod resolution;

pub use registry::{AddLauncherOptions, LauncherMeta, LauncherService};
pub(crate) use resolution::{parse_literal_launcher_command, resolve_direct_launcher_command};
pub use resolution::{build_launcher_command, resolve_launcher_name, resolve_launcher_run_dir};
