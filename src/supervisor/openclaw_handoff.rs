use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::SupervisorChildSpec;
#[cfg(test)]
use crate::infra::shell::OPENCLAW_NATIVE_SERVICE_IDENTITY_KEYS;
use crate::infra::shell::{
    apply_external_supervision_protocol_v1, apply_legacy_supervision, quote_posix,
};

const PROTOCOL: &str = "openclaw.gateway.restart-handoff";
const PROTOCOL_VERSION: u8 = 1;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RestartHandoffSupport {
    ProtocolV1,
    Unsupported(String),
    CleanupUnverified(String),
}

impl RestartHandoffSupport {
    pub(super) fn status_label(&self) -> &'static str {
        match self {
            Self::ProtocolV1 => "protocol-v1",
            Self::Unsupported(_) => "legacy",
            Self::CleanupUnverified(_) => unreachable!("unsettled helper cannot start a gateway"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RestartHandoffConsumeResult {
    Accepted,
    NotAccepted(String),
    Unsupported(String),
    Failed(String),
    NotChecked,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapabilitiesResponse {
    ok: bool,
    protocol: String,
    protocol_version: u8,
    operations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsumeResponse {
    ok: bool,
    protocol: String,
    protocol_version: u8,
    status: String,
    reason: Option<String>,
    handoff: Option<ConsumedHandoff>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsumedHandoff {
    pid: u64,
    supervisor_mode: String,
}

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

pub(super) fn probe_restart_handoff_support(spec: &SupervisorChildSpec) -> RestartHandoffSupport {
    if !spec.restart_handoff_pid_bound {
        return RestartHandoffSupport::Unsupported(
            "resolved command does not preserve the supervised gateway PID".to_string(),
        );
    }
    let probe_spec = prepare_supervisor_child_spec(
        spec,
        &RestartHandoffSupport::Unsupported("capability probe pending".to_string()),
    );
    let output = match run_openclaw_machine_command(
        &probe_spec,
        &["gateway", "restart-handoff", "capabilities", "--json"],
    ) {
        Ok(output) => output,
        Err(error) if error.cleanup_verified => {
            return RestartHandoffSupport::Unsupported(error.detail);
        }
        Err(error) => return RestartHandoffSupport::CleanupUnverified(error.detail),
    };
    if !output.status.success() {
        return RestartHandoffSupport::Unsupported(command_failure_detail(output.status));
    }
    let response = match serde_json::from_slice::<CapabilitiesResponse>(&output.stdout) {
        Ok(response) => response,
        Err(error) => {
            return RestartHandoffSupport::Unsupported(format!(
                "invalid capability response: {error}"
            ));
        }
    };
    if response.ok
        && response.protocol == PROTOCOL
        && response.protocol_version == PROTOCOL_VERSION
        && response
            .operations
            .iter()
            .any(|operation| operation == "consume")
    {
        RestartHandoffSupport::ProtocolV1
    } else {
        RestartHandoffSupport::Unsupported(
            "runtime does not advertise restart-handoff protocol version 1 consume".to_string(),
        )
    }
}

pub(super) fn prepare_supervisor_child_spec(
    spec: &SupervisorChildSpec,
    support: &RestartHandoffSupport,
) -> SupervisorChildSpec {
    let mut prepared = spec.clone();
    match support {
        RestartHandoffSupport::ProtocolV1 => {
            apply_external_supervision_protocol_v1(&mut prepared.process_env);
        }
        RestartHandoffSupport::Unsupported(_) | RestartHandoffSupport::CleanupUnverified(_) => {
            apply_legacy_supervision(&mut prepared.process_env);
        }
    }
    prepared
}

pub(super) fn consume_restart_handoff(
    spec: &SupervisorChildSpec,
    support: &RestartHandoffSupport,
    expected_pid: u32,
) -> RestartHandoffConsumeResult {
    if let RestartHandoffSupport::Unsupported(reason) = support {
        return RestartHandoffConsumeResult::Unsupported(reason.clone());
    }
    if let RestartHandoffSupport::CleanupUnverified(reason) = support {
        return RestartHandoffConsumeResult::Failed(reason.clone());
    }
    let prepared = prepare_supervisor_child_spec(spec, support);
    let expected_pid_arg = expected_pid.to_string();
    let output = match run_openclaw_machine_command(
        &prepared,
        &[
            "gateway",
            "restart-handoff",
            "consume",
            "--expected-pid",
            &expected_pid_arg,
            "--json",
        ],
    ) {
        Ok(output) => output,
        Err(error) => return RestartHandoffConsumeResult::Failed(error.detail),
    };
    if !output.status.success() {
        return RestartHandoffConsumeResult::Failed(command_failure_detail(output.status));
    }
    let response = match serde_json::from_slice::<ConsumeResponse>(&output.stdout) {
        Ok(response) => response,
        Err(error) => {
            return RestartHandoffConsumeResult::Failed(format!(
                "invalid consume response: {error}"
            ));
        }
    };
    if !response.ok
        || response.protocol != PROTOCOL
        || response.protocol_version != PROTOCOL_VERSION
    {
        return RestartHandoffConsumeResult::Failed(
            "consume response does not match restart-handoff protocol version 1".to_string(),
        );
    }
    match response.status.as_str() {
        "accepted" => match response.handoff {
            Some(handoff)
                if handoff.pid == u64::from(expected_pid)
                    && handoff.supervisor_mode == "external" =>
            {
                RestartHandoffConsumeResult::Accepted
            }
            _ => RestartHandoffConsumeResult::Failed(
                "accepted consume response does not match the exited external gateway".to_string(),
            ),
        },
        "none" | "rejected" => RestartHandoffConsumeResult::NotAccepted(
            response
                .reason
                .unwrap_or_else(|| response.status.to_string()),
        ),
        status => RestartHandoffConsumeResult::Failed(format!(
            "unknown consume response status \"{status}\""
        )),
    }
}

#[derive(Debug)]
struct MachineCommandError {
    detail: String,
    cleanup_verified: bool,
}

impl From<String> for MachineCommandError {
    fn from(detail: String) -> Self {
        Self {
            detail,
            cleanup_verified: true,
        }
    }
}

fn run_openclaw_machine_command(
    spec: &SupervisorChildSpec,
    openclaw_args: &[&str],
) -> Result<CommandOutput, MachineCommandError> {
    run_openclaw_machine_command_with_timeout(spec, openclaw_args, COMMAND_TIMEOUT)
}

fn run_openclaw_machine_command_with_timeout(
    spec: &SupervisorChildSpec,
    openclaw_args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, MachineCommandError> {
    let program_arguments = openclaw_program_arguments(spec, openclaw_args)?;
    let program = program_arguments
        .first()
        .ok_or_else(|| "restart-handoff command is missing a program".to_string())?;
    // Configure capture before spawning so every post-spawn failure settles the child.
    let (mut stdout, stdout_writer) = CommandCapture::new()
        .map_err(|error| format!("failed capturing restart-handoff stdout: {error}"))?;
    let (mut stderr, stderr_writer) = CommandCapture::new()
        .map_err(|error| format!("failed capturing restart-handoff stderr: {error}"))?;
    let mut command = Command::new(program);
    command
        .args(program_arguments.iter().skip(1))
        .stdin(Stdio::null())
        .stdout(stdout_writer)
        .stderr(stderr_writer)
        .env_clear()
        .envs(&spec.process_env)
        .current_dir(Path::new(&spec.run_dir));
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "failed running restart-handoff command for env \"{}\": {error}",
            spec.env_name
        )
    })?;
    // Command retains the parent-side writers even after spawn.
    drop(command);
    let deadline = Instant::now() + timeout;
    let result = collect_command(&mut child, &mut stdout, &mut stderr, deadline);
    match result {
        Ok(status) => Ok(CommandOutput {
            status,
            stdout: stdout.bytes,
        }),
        Err(detail) => {
            let cleanup_verified = terminate_command(&mut child, &mut stdout, &mut stderr);
            let detail = if cleanup_verified {
                detail
            } else {
                format!(
                    "{detail}; cleanup is unverified for restart-handoff helper {}; leaving the target stopped",
                    child.id()
                )
            };
            Err(MachineCommandError {
                detail,
                cleanup_verified,
            })
        }
    }
}

struct CommandCapture {
    pipe: io::PipeReader,
    bytes: Vec<u8>,
    eof: bool,
}

impl CommandCapture {
    fn new() -> io::Result<(Self, io::PipeWriter)> {
        let (pipe, writer) = io::pipe()?;
        #[cfg(unix)]
        {
            let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
            if flags == -1
                || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                    == -1
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok((
            Self {
                pipe,
                bytes: Vec::new(),
                eof: false,
            },
            writer,
        ))
    }

    fn drain(&mut self) -> io::Result<()> {
        // Limit work per stream so a continuously writing helper cannot starve
        // its other pipe or the operation deadline. Only retained bytes are capped.
        let mut chunk = [0_u8; 8192];
        for _ in 0..8 {
            if self.eof {
                break;
            }
            match self.read_available(&mut chunk) {
                Ok(0) => self.eof = true,
                Ok(count) => {
                    let remaining = MAX_OUTPUT_BYTES.saturating_sub(self.bytes.len());
                    self.bytes.extend_from_slice(&chunk[..count.min(remaining)]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_available(&mut self, chunk: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE;
            use windows_sys::Win32::System::Pipes::PeekNamedPipe;
            let mut available = 0;
            if unsafe {
                PeekNamedPipe(
                    self.pipe.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    Ok(0)
                } else {
                    Err(error)
                };
            }
            if available == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            // This is the sole reader; the bytes observed above cannot be consumed elsewhere.
            let count = chunk.len().min(available as usize);
            return self.pipe.read(&mut chunk[..count]);
        }
        #[cfg(not(windows))]
        self.pipe.read(chunk)
    }
}

fn collect_command(
    child: &mut Child,
    stdout: &mut CommandCapture,
    stderr: &mut CommandCapture,
    deadline: Instant,
) -> Result<ExitStatus, String> {
    loop {
        stdout
            .drain()
            .map_err(|error| format!("failed reading restart-handoff stdout: {error}"))?;
        stderr
            .drain()
            .map_err(|error| format!("failed reading restart-handoff stderr: {error}"))?;
        // Do not reap the group leader while output is outstanding. Its PID
        // reserves the process-group identity until timeout cleanup has signaled it.
        if stdout.eof && stderr.eof {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {}
                Err(error) => {
                    return Err(format!(
                        "failed waiting for restart-handoff command: {error}"
                    ));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(
                "restart-handoff command timed out waiting for completion and output".to_string(),
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_command(
    child: &mut Child,
    stdout: &mut CommandCapture,
    stderr: &mut CommandCapture,
) -> bool {
    #[cfg(unix)]
    {
        // The leader is still unreaped. Escalate even when it exited already:
        // descendants can retain either output pipe after the leader's exit.
        let group = -(child.id() as libc::pid_t);
        unsafe {
            libc::kill(group, libc::SIGTERM);
        }
        thread::sleep(Duration::from_millis(500));
        unsafe {
            libc::kill(group, libc::SIGKILL);
        }
    }
    // Also address a direct helper that left its original process group.
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut reaped = false;
    let mut capture_ok = true;
    loop {
        // Always settle both captures; dropping the handles closes our readers,
        // even if an escaped descendant prevents EOF. No reader threads outlive us.
        capture_ok &= stdout.drain().is_ok();
        capture_ok &= stderr.drain().is_ok();
        if !reaped {
            reaped = matches!(child.try_wait(), Ok(Some(_)));
        }
        #[cfg(unix)]
        let group_settled = (unsafe { libc::kill(-(child.id() as libc::pid_t), 0) }) == -1
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        #[cfg(not(unix))]
        let group_settled = true;
        if reaped && stdout.eof && stderr.eof && group_settled {
            // macOS can report EPERM for signals sent to a zombie group.
            // Settlement, not signal delivery alone, decides safe fallback.
            return capture_ok;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn openclaw_program_arguments(
    spec: &SupervisorChildSpec,
    openclaw_args: &[&str],
) -> Result<Vec<String>, String> {
    let gateway_args = [
        "gateway".to_string(),
        "run".to_string(),
        "--port".to_string(),
        spec.child_port.to_string(),
    ];
    if let Some(binary_path) = spec.binary_path.as_ref() {
        let prefix_len = spec
            .args
            .len()
            .checked_sub(gateway_args.len())
            .ok_or_else(|| {
                format!(
                    "service child env \"{}\" has incomplete gateway arguments",
                    spec.env_name
                )
            })?;
        if spec.args[prefix_len..] != gateway_args {
            return Err(format!(
                "service child env \"{}\" gateway arguments do not match its port",
                spec.env_name
            ));
        }
        let mut program_arguments = vec![binary_path.clone()];
        program_arguments.extend(spec.args[..prefix_len].iter().cloned());
        program_arguments.extend(openclaw_args.iter().map(|arg| (*arg).to_string()));
        return Ok(program_arguments);
    }

    let command = spec.command.as_deref().ok_or_else(|| {
        format!(
            "service child env \"{}\" is missing a launcher command",
            spec.env_name
        )
    })?;
    let suffix = format!(
        " {}",
        gateway_args
            .iter()
            .map(|arg| quote_posix(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let base_command = command.strip_suffix(&suffix).ok_or_else(|| {
        format!(
            "service child env \"{}\" launcher command does not end with its gateway arguments",
            spec.env_name
        )
    })?;
    let machine_args = openclaw_args
        .iter()
        .map(|arg| quote_posix(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let machine_command = format!("{base_command} {machine_args}");
    if cfg!(windows) {
        Ok(vec!["cmd".to_string(), "/C".to_string(), machine_command])
    } else {
        Ok(vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            machine_command,
        ])
    }
}

fn command_failure_detail(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("command exited {code}"),
        None => "command terminated without an exit code".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn direct_spec() -> SupervisorChildSpec {
        SupervisorChildSpec {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: "local".to_string(),
            restart_handoff_pid_bound: true,
            command: None,
            binary_path: Some("/runtime/node_modules/openclaw/openclaw.mjs".to_string()),
            runtime_source_kind: Some("installed".to_string()),
            runtime_release_version: None,
            runtime_release_channel: None,
            args: vec![
                "--profile".to_string(),
                "demo".to_string(),
                "gateway".to_string(),
                "run".to_string(),
                "--port".to_string(),
                "19001".to_string(),
            ],
            run_dir: "/tmp/demo".to_string(),
            child_port: 19001,
            stdout_path: "/tmp/demo.stdout.log".to_string(),
            stderr_path: "/tmp/demo.stderr.log".to_string(),
            process_env: BTreeMap::from([
                (
                    "OPENCLAW_LAUNCHD_LABEL".to_string(),
                    "ai.openclaw.ocm".to_string(),
                ),
                (
                    "OPENCLAW_SYSTEMD_UNIT".to_string(),
                    "ai.openclaw.ocm.service".to_string(),
                ),
                (
                    "OPENCLAW_WINDOWS_TASK_NAME".to_string(),
                    "OCM Supervisor".to_string(),
                ),
                (
                    "OPENCLAW_SERVICE_MARKER".to_string(),
                    "openclaw".to_string(),
                ),
                ("OPENCLAW_SERVICE_KIND".to_string(), "gateway".to_string()),
            ]),
        }
    }

    #[cfg(unix)]
    fn shell_spec(script: &str) -> SupervisorChildSpec {
        let mut spec = direct_spec();
        spec.binary_path = Some("/bin/sh".to_string());
        spec.args = vec![
            "-c".to_string(),
            script.to_string(),
            "helper".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            "19001".to_string(),
        ];
        spec.run_dir = std::env::temp_dir().to_string_lossy().into_owned();
        spec
    }

    #[cfg(unix)]
    #[test]
    fn machine_command_bounds_inherited_stdout_and_stderr_and_reaps_leader() {
        for redirect in ["2>/dev/null", ">/dev/null"] {
            let dir = tempfile::tempdir().unwrap();
            let leader = dir.path().join("leader");
            let holder = dir.path().join("holder");
            let script = format!(
                "echo $$ > '{}'; /bin/sleep 60 {redirect} & echo $! > '{}'; printf 'valid response'; exit 0",
                leader.display(),
                holder.display()
            );
            let started = Instant::now();
            let error = run_openclaw_machine_command_with_timeout(
                &shell_spec(&script),
                &["capabilities"],
                Duration::from_millis(100),
            )
            .err()
            .unwrap();
            assert!(started.elapsed() < Duration::from_secs(3));
            assert!(error.cleanup_verified, "{}", error.detail);
            let leader: i32 = std::fs::read_to_string(leader)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert_eq!(
                unsafe { libc::waitpid(leader, std::ptr::null_mut(), libc::WNOHANG) },
                -1,
                "leader was not reaped"
            );
            let holder: i32 = std::fs::read_to_string(holder)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            // Orphan zombies may await the host reaper, but they must no longer own output.
            assert!(error.detail.contains("timed out"));
            assert!(
                crate::infra::process_identity::observe_process(holder as u32)
                    .unwrap()
                    .is_none_or(|process| !process.running),
                "owned holder is still running"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn machine_command_drains_both_streams_after_capture_limit() {
        let spec =
            shell_spec("/usr/bin/head -c 131072 /dev/zero; /usr/bin/head -c 131072 /dev/zero >&2");
        let output = run_openclaw_machine_command_with_timeout(
            &spec,
            &["capabilities"],
            Duration::from_secs(3),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_OUTPUT_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn machine_command_settles_direct_timeout() {
        let error = run_openclaw_machine_command_with_timeout(
            &shell_spec("exec /bin/sleep 60"),
            &["capabilities"],
            Duration::from_millis(100),
        )
        .err()
        .unwrap();
        assert!(error.cleanup_verified, "{}", error.detail);
    }

    #[cfg(unix)]
    #[test]
    fn machine_command_preserves_unsupported_and_valid_helpers() {
        for (script, code, stdout) in [
            ("printf valid", 0, b"valid".as_slice()),
            ("exit 64", 64, b"".as_slice()),
        ] {
            let output = run_openclaw_machine_command_with_timeout(
                &shell_spec(script),
                &["capabilities"],
                Duration::from_secs(1),
            )
            .unwrap();
            assert_eq!(output.status.code(), Some(code));
            assert_eq!(output.stdout, stdout);
        }
    }

    #[test]
    fn direct_binding_replaces_only_gateway_arguments() {
        let args = openclaw_program_arguments(
            &direct_spec(),
            &[
                "gateway",
                "restart-handoff",
                "consume",
                "--expected-pid",
                "42",
                "--json",
            ],
        )
        .unwrap();
        assert_eq!(
            args,
            vec![
                "/runtime/node_modules/openclaw/openclaw.mjs",
                "--profile",
                "demo",
                "gateway",
                "restart-handoff",
                "consume",
                "--expected-pid",
                "42",
                "--json",
            ]
        );
    }

    #[test]
    fn wrapper_binding_skips_restart_handoff_capability_probe() {
        let mut spec = direct_spec();
        spec.restart_handoff_pid_bound = false;
        spec.binary_path = Some("/missing/pnpm".to_string());

        assert_eq!(
            probe_restart_handoff_support(&spec),
            RestartHandoffSupport::Unsupported(
                "resolved command does not preserve the supervised gateway PID".to_string()
            )
        );
    }

    #[test]
    fn supported_child_uses_external_supervision_without_native_identity() {
        let prepared =
            prepare_supervisor_child_spec(&direct_spec(), &RestartHandoffSupport::ProtocolV1);
        assert_eq!(
            prepared
                .process_env
                .get("OPENCLAW_SUPERVISOR_MODE")
                .map(String::as_str),
            Some("external")
        );
        assert_eq!(
            prepared
                .process_env
                .get("OPENCLAW_SERVICE_KIND")
                .map(String::as_str),
            Some("gateway")
        );
        assert!(!prepared.process_env.contains_key("OPENCLAW_NO_RESPAWN"));
        for key in OPENCLAW_NATIVE_SERVICE_IDENTITY_KEYS {
            assert!(!prepared.process_env.contains_key(key));
        }
    }

    #[test]
    fn unsupported_child_blocks_detached_respawn_and_native_identity_inference() {
        let prepared = prepare_supervisor_child_spec(
            &direct_spec(),
            &RestartHandoffSupport::Unsupported("missing".to_string()),
        );
        assert_eq!(
            prepared
                .process_env
                .get("OPENCLAW_SERVICE_MARKER")
                .map(String::as_str),
            Some("openclaw")
        );
        assert!(!prepared.process_env.contains_key("OPENCLAW_SERVICE_KIND"));
        assert!(
            !prepared
                .process_env
                .contains_key("OPENCLAW_SUPERVISOR_MODE")
        );
        assert_eq!(
            prepared
                .process_env
                .get("OPENCLAW_NO_RESPAWN")
                .map(String::as_str),
            Some("1")
        );
        for key in OPENCLAW_NATIVE_SERVICE_IDENTITY_KEYS {
            assert!(!prepared.process_env.contains_key(key));
        }
    }
}
