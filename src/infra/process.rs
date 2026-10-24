use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

pub fn run_direct(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<i32, String> {
    run_reporting_as(command, args, env, cwd, &format!("\"{command}\""))
}

/// Run `command` through the platform shell. `subject` names what ran in a
/// signal diagnostic, because the shell may replace itself with the command.
pub fn run_shell(
    command: &str,
    subject: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<i32, String> {
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-lc")
    };
    run_reporting_as(
        shell,
        &[flag.to_string(), command.to_string()],
        env,
        cwd,
        subject,
    )
}

fn run_reporting_as(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
    subject: &str,
) -> Result<i32, String> {
    let status = Command::new(command)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .status()
        .map_err(|error| format!("failed to run \"{command}\": {error}"))?;
    if let Some(diagnostic) = signal_diagnostic(subject, &status) {
        let _ = writeln!(io::stderr().lock(), "{diagnostic}");
    }
    Ok(exit_status_code(&status))
}

/// Report a child's exit the way a shell does: its exit code, or 128 plus the
/// signal number when a signal terminated it.
fn exit_status_code(status: &ExitStatus) -> i32 {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
        return 128 + signal;
    }
    status.code().unwrap_or(1)
}

/// Describe a signal death on stderr, as a shell does. Like shells, stay quiet
/// for SIGINT and SIGPIPE, which usually come from the terminal or the pipeline.
#[cfg(unix)]
fn signal_diagnostic(subject: &str, status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;

    let signal = status.signal()?;
    if signal == libc::SIGINT || signal == libc::SIGPIPE {
        return None;
    }
    let name = signal_name(signal)
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    let core = if status.core_dumped() {
        " (core dumped)"
    } else {
        ""
    };
    Some(format!(
        "ocm: {subject} was terminated by signal {signal}{name}{core}"
    ))
}

#[cfg(not(unix))]
fn signal_diagnostic(_subject: &str, _status: &ExitStatus) -> Option<String> {
    None
}

#[cfg(unix)]
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        _ => return None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::{exit_status_code, signal_diagnostic};

    #[test]
    fn numeric_exit_codes_are_reported_unchanged() {
        for code in [0, 3, 42, 255] {
            let status = ExitStatus::from_raw(code << 8);
            assert_eq!(exit_status_code(&status), code);
            assert_eq!(signal_diagnostic("\"openclaw\"", &status), None);
        }
    }

    #[test]
    fn signal_deaths_are_reported_as_128_plus_the_signal() {
        for signal in [
            libc::SIGTERM,
            libc::SIGKILL,
            libc::SIGINT,
            libc::SIGSEGV,
            libc::SIGABRT,
        ] {
            let status = ExitStatus::from_raw(signal);
            assert_eq!(exit_status_code(&status), 128 + signal);
        }
    }

    #[test]
    fn signal_deaths_are_described_except_terminal_and_pipeline_signals() {
        assert_eq!(
            signal_diagnostic("\"openclaw\"", &ExitStatus::from_raw(libc::SIGKILL)).as_deref(),
            Some(&*format!(
                "ocm: \"openclaw\" was terminated by signal {} (SIGKILL)",
                libc::SIGKILL
            ))
        );
        assert_eq!(
            signal_diagnostic(
                "launcher \"dev\"",
                &ExitStatus::from_raw(libc::SIGSEGV | 0x80)
            )
            .as_deref(),
            Some(&*format!(
                "ocm: launcher \"dev\" was terminated by signal {} (SIGSEGV) (core dumped)",
                libc::SIGSEGV
            ))
        );
        assert_eq!(
            signal_diagnostic("\"openclaw\"", &ExitStatus::from_raw(libc::SIGINT)),
            None
        );
        assert_eq!(
            signal_diagnostic("\"openclaw\"", &ExitStatus::from_raw(libc::SIGPIPE)),
            None
        );
    }
}
