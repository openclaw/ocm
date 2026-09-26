//! Run an OCM worker outside the requesting Gateway's process containment.
use std::path::Path;
use std::process::Command;

#[cfg(unix)]
pub(super) fn command(executable: &Path, name: &str) -> Result<Command, String> {
    use std::os::unix::process::CommandExt;

    #[cfg(target_os = "linux")]
    let in_service = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|error| format!("cannot inspect worker containment: {error}"))?
        .split('/')
        .any(|component| component.trim_end() == "ai.openclaw.ocm.service");
    #[cfg(not(target_os = "linux"))]
    let in_service = false;

    let mut command = if in_service {
        // A process group cannot escape systemd's KillMode=control-group.
        let mut scope = Command::new("systemd-run");
        scope
            .args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit={name}"))
            .arg("--")
            .arg(executable);
        scope
    } else {
        Command::new(executable)
    };
    // SAFETY: only async-signal-safe syscalls run in the post-fork child.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    Ok(command)
}

#[cfg(not(unix))]
pub(super) fn command(_executable: &Path, _name: &str) -> Result<Command, String> {
    Err("detached OCM workers are unsupported on this platform".into())
}
