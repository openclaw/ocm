mod registry;
mod resolution;
mod source;

pub use registry::{AddLauncherOptions, LauncherMeta, LauncherService};
pub use resolution::{
    build_launcher_command, launcher_working_directory_issue, resolve_launcher_name,
    resolve_launcher_run_dir, working_directory_issue,
};
pub(crate) use resolution::{parse_literal_launcher_command, resolve_direct_launcher_command};

pub(crate) use source::{launcher_source_root, read_source_json};
