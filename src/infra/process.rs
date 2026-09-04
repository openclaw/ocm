use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn run_direct(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
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
    Ok(status.code().unwrap_or(1))
}

pub fn run_shell(command: &str, env: &BTreeMap<String, String>, cwd: &Path) -> Result<i32, String> {
    if cfg!(windows) {
        run_direct("cmd", &["/C".to_string(), command.to_string()], env, cwd)
    } else {
        run_direct("sh", &["-lc".to_string(), command.to_string()], env, cwd)
    }
}

/// Run a command, capture output, and kill the child if it exceeds `timeout`.
pub(crate) fn command_output(
    mut command: Command,
    timeout: Duration,
    label: &str,
) -> Result<Output, String> {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to run {label}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{label} stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{label} stderr was not captured"))?;

    let stdout_reader = thread::spawn(move || read_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_pipe(stderr));

    let status = wait_for_child(&mut child, timeout, label);
    let stdout = stdout_reader
        .join()
        .map_err(|_| format!("{label} stdout reader panicked"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| format!("{label} stderr reader panicked"))?;
    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

fn read_pipe(mut reader: impl std::io::Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    buf
}

pub(crate) fn wait_for_child(
    child: &mut Child,
    timeout: Duration,
    label: &str,
) -> Result<ExitStatus, String> {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started_at.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                terminate_child(child);
                return Err(format!("{label} timed out after {timeout:?}"));
            }
            Err(error) => {
                terminate_child(child);
                return Err(format!("failed waiting for {label}: {error}"));
            }
        }
    }
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("kill")
            .args(["-TERM", "--", &process_group])
            .status();
        for _ in 0..20 {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = Command::new("kill")
            .args(["-KILL", "--", &process_group])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}
