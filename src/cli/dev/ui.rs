use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use url::Url;

use super::*;
use crate::env::{DevUiChildRole, SourceUiTarget};

#[derive(Clone, Debug)]
struct DevUiTarget {
    port: u32,
    gateway_url: String,
}

impl DevUiTarget {
    fn validate(&self, gateway_port: u32) -> Result<(), String> {
        let url = Url::parse(&self.gateway_url)
            .map_err(|_| "the saved UI Gateway URL is invalid".to_string())?;
        if !(1..=u16::MAX as u32).contains(&self.port)
            || !(1..=u16::MAX as u32).contains(&gateway_port)
            || url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port_or_known_default() != Some(gateway_port as u16)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "the saved UI target does not match the dev session's loopback Gateway".to_string(),
            );
        }
        Ok(())
    }

    fn documents_ready(&self) -> bool {
        let Ok(gateway) = Url::parse(&self.gateway_url) else {
            return false;
        };
        let Some(port) = gateway.port_or_known_default() else {
            return false;
        };
        http_ready(u32::from(port), "/health", false)
            && http_ready(u32::from(port), gateway.path(), true)
            && http_ready(self.port, "/", true)
    }

    fn command(&self, meta: &EnvMeta, source: &Path, env: &BTreeMap<String, String>) -> Command {
        let args = [
            "scripts/ui.js",
            "dev",
            "--host",
            "127.0.0.1",
            "--port",
            &self.port.to_string(),
            "--strictPort",
            "--logLevel",
            "error",
            "--clearScreen",
            "false",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let mut command = gated_dev_command("node", &args);
        command
            .stdin(Stdio::null())
            .env_clear()
            .envs(build_openclaw_dev_source_env(meta, env, source))
            .env("OPENCLAW_UI_DEV_GATEWAY_URL", &self.gateway_url)
            .env("OPENCLAW_CONTROL_UI_BASE_PATH", "/")
            .current_dir(source);
        command
    }

    fn dashboard_command(
        &self,
        meta: &EnvMeta,
        source: &Path,
        env: &BTreeMap<String, String>,
    ) -> Command {
        let args = vec![
            display_path(&source.join("openclaw.mjs")),
            "dashboard".to_string(),
            "--json".to_string(),
        ];
        let mut command = gated_dev_command("node", &args);
        command
            .stdin(Stdio::null())
            .env_clear()
            .envs(build_openclaw_dev_source_env(meta, env, source))
            .current_dir(source);
        command
    }

    fn handoff_url(&self, output: &[u8]) -> Result<String, String> {
        let payload: Value = serde_json::from_slice(output)
            .map_err(|_| "the native dashboard did not return valid JSON".to_string())?;
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("the native dashboard did not produce a browser handoff".to_string());
        }
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
        if payload
            .get("browserBootstrapExpiresAtMs")
            .and_then(Value::as_i64)
            .is_none_or(|expires| i128::from(expires) <= now_ms)
        {
            return Err(
                "the native dashboard handoff is missing or expired; restart dev to request a fresh link"
                    .to_string(),
            );
        }
        let mut link = payload
            .get("browserUrl")
            .and_then(Value::as_str)
            .and_then(|value| Url::parse(value).ok())
            .ok_or_else(|| {
                "the native dashboard did not produce a valid browser URL".to_string()
            })?;
        let expected = Url::parse(&self.gateway_url)
            .map_err(|_| "the dev Gateway URL is invalid".to_string())?;
        if link.origin() != expected.origin()
            || link.path().trim_end_matches('/') != expected.path().trim_end_matches('/')
            || !link.username().is_empty()
            || link.password().is_some()
            || link.query().is_some()
        {
            return Err("the native dashboard targets a different Gateway; stop the dev session before changing its address".to_string());
        }
        let fragment = link.fragment().ok_or_else(|| {
            "the native dashboard did not provide its normal owner handoff".to_string()
        })?;
        let fields = url::form_urlencoded::parse(fragment.as_bytes()).collect::<Vec<_>>();
        let gateways = fields
            .iter()
            .filter(|(key, _)| key == "gatewayUrl")
            .collect::<Vec<_>>();
        let grants = fields
            .iter()
            .filter(|(key, _)| key == "bootstrapToken")
            .collect::<Vec<_>>();
        if gateways.len() != 1 || grants.len() != 1 || grants[0].1.trim().is_empty() {
            return Err(
                "the native dashboard did not provide an unambiguous Gateway owner handoff"
                    .to_string(),
            );
        }
        let mut gateway = Url::parse(&gateways[0].1)
            .map_err(|_| "the native dashboard Gateway URL is invalid".to_string())?;
        if gateway.scheme() != "ws"
            || !gateway.username().is_empty()
            || gateway.password().is_some()
            || gateway.query().is_some()
            || gateway.fragment().is_some()
        {
            return Err(
                "the native dashboard Gateway URL does not match this dev session".to_string(),
            );
        }
        gateway
            .set_scheme("http")
            .map_err(|_| "the native dashboard Gateway URL is invalid".to_string())?;
        if gateway.as_str().trim_end_matches('/') != expected.as_str().trim_end_matches('/') {
            return Err("the native dashboard targets a different Gateway; stop the dev session before changing its address".to_string());
        }
        // Preserve the native grant and logical Gateway identity exactly. Only the
        // document moves to Vite, whose transport is owned by OpenClaw.
        link.set_port(Some(self.port as u16))
            .map_err(|_| "the session UI port is invalid".to_string())?;
        link.set_path("/");
        Ok(link.to_string())
    }
}

pub(super) fn http_ready(port: u32, path: &str, html: bool) -> bool {
    if !(1..=u16::MAX as u32).contains(&port) {
        return false;
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(350);
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port as u16);
    let Ok(mut stream) = TcpStream::connect_timeout(&address.into(), Duration::from_millis(100))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    if stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .is_err()
    {
        return false;
    }
    let mut response = Vec::new();
    let mut chunk = [0_u8; 1024];
    while response.len() < 8192 {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(remaining));
        let Ok(count) = stream.read(&mut chunk) else {
            return false;
        };
        if count == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..count]);
        if response.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            break;
        }
    }
    if !response.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        return false;
    }
    let headers = String::from_utf8_lossy(&response).to_ascii_lowercase();
    (headers.starts_with("http/1.1 200 ") || headers.starts_with("http/1.0 200 "))
        && (!html
            || headers
                .lines()
                .any(|line| line.starts_with("content-type:") && line.contains("text/html")))
}

const DASHBOARD_TIMEOUT: Duration = Duration::from_secs(30);
const DASHBOARD_ATTEMPTS: usize = 3;

enum UiChildOutput {
    Capture,
    Logs(SourceWatchLogFiles),
}

struct OwnedUiChild {
    role: DevUiChildRole,
    child: std::process::Child,
    guard: SourceWatchProcessGuard,
    identity: ProcessIdentity,
    started: Option<std::time::Instant>,
    completion: Option<SourceWatchResult<std::process::ExitStatus>>,
    stdout: Option<JoinHandle<SourceWatchResult<Vec<u8>>>>,
    stderr: Option<JoinHandle<SourceWatchResult<Vec<u8>>>>,
    tees: Vec<JoinHandle<SourceWatchResult<()>>>,
    drained_output: Option<SourceWatchResult<(Vec<u8>, Vec<u8>)>>,
}

fn gated_dev_command(program: &str, args: &[String]) -> Command {
    #[cfg(unix)]
    {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", SOURCE_WATCH_SETUP_SHIM, "ocm-dev", program]);
        command.args(args);
        command
    }
    #[cfg(not(unix))]
    {
        let mut command = Command::new(program);
        command.args(args);
        command
    }
}

fn spawn_ui_child(
    mut command: Command,
    role: DevUiChildRole,
    terminal: bool,
    output: UiChildOutput,
    lease: &mut SourceWatchLease,
) -> SourceWatchResult<OwnedUiChild> {
    lease.configure_child(&mut command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut guard = SourceWatchProcessGuard::new_with_terminal(terminal)?;
    guard.configure_command(&mut command)?;
    lease.begin_ui_child_spawn(role)?;
    let mut child = command.spawn().map_err(|error| {
        let message = format!(
            "failed to run {:?} for dev {role:?}: {error}",
            command.get_program()
        );
        match lease.clear_ui_child(role, None) {
            Ok(()) => SourceWatchError::from(message),
            Err(error) => SourceWatchError::unverified(format!("{message}; {error}")),
        }
    })?;
    let ownership = (|| {
        guard.assign_child(&child)?;
        lease.attach_to_child(&child)?;
        lease.record_ui_child(role, child.id())
    })();
    let identity = match ownership {
        Ok(identity) => identity,
        Err(error) => {
            #[cfg(windows)]
            let stopped = stop_suspended_source_watch_after_error(&mut child, &guard, error);
            #[cfg(not(windows))]
            let stopped = stop_source_watch_after_error(&mut child, &guard, error);
            if stopped.cleanup_verified {
                let expected = lease
                    .session()
                    .and_then(|session| session.ui.as_ref())
                    .and_then(|ui| ui.children.get(role))
                    .cloned();
                lease.clear_ui_child(role, expected.as_ref())?;
            }
            return Err(stopped);
        }
    };
    let stdout = child
        .stdout
        .take()
        .expect("dev stdout was configured as a pipe");
    let stderr = child
        .stderr
        .take()
        .expect("dev stderr was configured as a pipe");
    let (stdout, stderr, tees) = match output {
        UiChildOutput::Capture => (
            Some(spawn_source_capture(stdout, "stdout", false)),
            Some(spawn_source_capture(stderr, "stderr", false)),
            Vec::new(),
        ),
        UiChildOutput::Logs(logs) => (
            None,
            None,
            vec![
                spawn_tee_thread(stdout, io::stdout(), logs.stdout, "stdout"),
                spawn_tee_thread(stderr, io::stderr(), logs.stderr, "stderr"),
            ],
        ),
    };
    Ok(OwnedUiChild {
        role,
        child,
        guard,
        identity,
        started: None,
        completion: None,
        stdout,
        stderr,
        tees,
        drained_output: None,
    })
}

impl OwnedUiChild {
    fn start(&mut self) -> SourceWatchResult<()> {
        self.guard.start_child(&self.child)?;
        self.started = Some(std::time::Instant::now());
        Ok(())
    }

    fn poll(&mut self) -> SourceWatchResult<Option<std::process::ExitStatus>> {
        if self.completion.is_some() {
            return Ok(None);
        }
        let result = poll_source_watch_child(&mut self.child, &self.guard)?;
        if let Some(status) = result {
            self.completion = Some(self.guard.classify_completion(Ok(status), true, false));
        }
        Ok(result)
    }

    fn stop(&mut self) -> SourceWatchResult<()> {
        if self.completion.is_some() {
            return Ok(());
        }
        if self.role == DevUiChildRole::Command
            && let Some(started) = self.started
        {
            // The original request budget also bounds stop-time grace. Native
            // dashboard reads can finish normally after the Gateway stops.
            while started.elapsed() < DASHBOARD_TIMEOUT {
                if self.poll()?.is_some() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        let status = if self.started.is_some() {
            stop_source_watch_child(&mut self.child, &self.guard)?
        } else {
            // Startup did not complete. The guard retains whether release was
            // attempted while we terminate the assigned scope directly.
            self.guard
                .stop_remaining(self.child.id())
                .map_err(SourceWatchError::unverified)?;
            self.child
                .wait()
                .map_err(|error| SourceWatchError::unverified(error.to_string()))?
        };
        // Every role owns both output pipes. Keep the raw completion separate
        // from their EOF result, including a partially released startup gate.
        self.completion = Some(self.guard.classify_completion(Ok(status), true, true));
        Ok(())
    }

    fn finish(&mut self, lease: &mut SourceWatchLease) -> SourceWatchResult<()> {
        let Some(completion) = self.completion.as_ref() else {
            return Err(SourceWatchError::unverified(
                "dev child cleanup has not been verified",
            ));
        };
        let terminal = self.guard.restore_terminal();
        if self.drained_output.is_none() {
            let captures = collect_source_captures(self.stdout.take(), self.stderr.take());
            let tees = wait_for_tee_threads(std::mem::take(&mut self.tees));
            let drained = match (captures, tees) {
                (Ok(output), Ok(())) => Ok(output),
                (Err(capture), Err(tee)) => Err(capture.combine(tee)),
                (Err(error), _) | (_, Err(error)) => Err(error),
            };
            // A later cleanup pass must not turn a consumed failed reader into
            // successful EOF and erase this child's retained ownership.
            self.drained_output = Some(drained);
        }
        let drained = match self.drained_output.as_ref().expect("output was drained") {
            Ok(_) => Ok(()),
            // A bounded reporting failure after verified EOF is delivered as
            // a pending handoff through output(); it is not a component exit.
            Err(error) if self.role == DevUiChildRole::Command && error.cleanup_verified => Ok(()),
            Err(error) => Err(error.clone()),
        };
        let mut result =
            combine_source_watch_cleanup_results(completion.clone(), drained, Ok(())).map(|_| ());
        if source_watch_allows_service_restore(&result)
            && let Err(error) = lease.clear_ui_child(self.role, Some(&self.identity))
        {
            result = Err(match result {
                Ok(()) => error.into(),
                Err(prior) => prior.combine(error.into()),
            });
        }
        match (result, terminal) {
            (result, Ok(())) => result,
            (Ok(()), Err(error)) => Err(error.into()),
            (Err(prior), Err(error)) => Err(prior.combine(error.into())),
        }
    }

    fn output(&mut self) -> Result<(Vec<u8>, Vec<u8>), String> {
        match &mut self.drained_output {
            Some(Ok((stdout, stderr))) => Ok((std::mem::take(stdout), std::mem::take(stderr))),
            Some(Err(error)) => Err(error.message.clone()),
            None => Err("dev output has not completed cleanup verification".to_string()),
        }
    }
}

impl Cli {
    pub(super) fn inspect_dev_ui_status(
        &self,
        meta: &EnvMeta,
        observation: &Result<SourceWatchState, String>,
    ) -> Option<DevUiStatusSummary> {
        let service = self.environment_service();
        let active = match observation {
            Ok(SourceWatchState::Active(active)) if active.ui.is_some() => active,
            Err(error) => {
                let session = service.source_watch_session(&meta.name).ok().flatten()?;
                if session.closed {
                    return None;
                }
                let ui = session.ui.as_ref()?;
                let target = ui.target.as_ref()?;
                return Some(DevUiStatusSummary {
                    port: target.port,
                    url: format!("http://127.0.0.1:{}/", target.port),
                    pid: ui.children.get(DevUiChildRole::Ui).map(|child| child.pid),
                    process_running: None,
                    http_ready: None,
                    issue: Some(error.clone()),
                });
            }
            _ => return None,
        };
        let endpoint = active.ui.as_ref()?;
        let mut summary = DevUiStatusSummary {
            port: endpoint.port,
            url: format!("http://127.0.0.1:{}/", endpoint.port),
            pid: Some(endpoint.pid),
            process_running: None,
            http_ready: None,
            issue: None,
        };
        let inspected = (|| -> Result<bool, String> {
            let session = service
                .source_watch_session(&meta.name)?
                .ok_or_else(|| "dev UI session is no longer available".to_string())?;
            if session.closed
                || active.env_name != meta.name
                || !session.restore_target_matches(meta)
                || session.process_scope != process_scope_id()?
            {
                return Err("dev UI session identity or process scope changed".to_string());
            }
            if let Some(error) = session.unsafe_cleanup_error() {
                return Err(error.to_string());
            }
            let ui = session
                .ui
                .as_ref()
                .ok_or_else(|| "dev UI session no longer records a UI owner".to_string())?;
            let target = ui
                .target
                .as_ref()
                .ok_or_else(|| "dev UI session no longer records its target".to_string())?;
            let child = ui
                .children
                .get(DevUiChildRole::Ui)
                .ok_or_else(|| "dev UI session no longer records its process".to_string())?;
            let generation = active
                .token
                .strip_prefix("lease:")
                .and_then(|token| token.split_once(':'))
                .map(|(generation, _)| generation);
            if generation != Some(session.lease_id.as_str())
                || active.watching != session.watching
                || target.port != endpoint.port
                || target.gateway_url != endpoint.gateway_url
                || child.pid != endpoint.pid
                || ui
                    .children
                    .get(DevUiChildRole::Gateway)
                    .map(|child| child.pid)
                    != Some(active.watch_pid)
            {
                return Err("dev UI generation, target or recorded process changed".to_string());
            }
            Ok(observe_process(child.pid)?
                .is_some_and(|process| process.running && process.identity == *child))
        })();
        match inspected {
            Ok(running) => {
                summary.process_running = Some(running);
                summary.http_ready = Some(running && http_ready(endpoint.port, "/", true));
            }
            Err(error) => summary.issue = Some(error),
        }
        Some(summary)
    }

    pub(super) fn prepare_dev_ui(
        &self,
        meta: &EnvMeta,
        source: &Path,
        lease: &mut SourceWatchLease,
        stop: &AtomicBool,
    ) -> SourceWatchResult<()> {
        if source_watch_cancelled(lease, stop)? {
            return Err("dev UI setup was cancelled".to_string().into());
        }
        let gateway_url = crate::store::dev_ui_gateway_url(
            &derive_env_paths(Path::new(&meta.root)),
            meta.gateway_port.unwrap_or_default(),
        )?;
        if !source.join("scripts/ui.js").is_file() || !source.join("ui/index.html").is_file() {
            return Err("the selected source does not contain the native Control UI development entry point".to_string().into());
        }
        const PROBE: &str = r#"const fs = require('node:fs');
const path = require('node:path');
const {createRequire} = require('node:module');
const root = fs.realpathSync(process.cwd());
const uiRequire = createRequire(path.join(root, 'ui', 'package.json'));
for (const name of ['vite', 'dompurify']) {
  const entry = fs.realpathSync(uiRequire.resolve(name));
  const relative = path.relative(root, entry);
  if (path.isAbsolute(relative) || relative === '..' || relative.startsWith('..' + path.sep)) {
    throw new Error(name + ' resolves outside the selected OpenClaw checkout');
  }
}"#;
        let mut env = build_openclaw_dev_source_env(meta, &self.env, source);
        for key in ["NODE_OPTIONS", "NODE_PATH", "NODE_COMPILE_CACHE"] {
            env.remove(key);
        }
        let mut probe = gated_dev_command(
            "node",
            &[
                "--input-type=commonjs".to_string(),
                "--eval".to_string(),
                PROBE.to_string(),
            ],
        );
        probe
            .env_clear()
            .envs(env)
            .current_dir(source)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = self
            .run_owned_source_watch_command(
                probe,
                lease,
                stop,
                SourcePreparationCommand::DependencyProbe,
            )?
            .ok_or_else(|| SourceWatchError::from("dev UI setup was cancelled".to_string()))?;
        if !output.status.success() {
            return Err("Control UI dependencies are not ready; run pnpm install --frozen-lockfile in the selected checkout before retrying --ui".to_string().into());
        }
        let service = self.environment_service();
        let _operation = service.lock_operation(&meta.name)?;
        // Only startup reads other live session claims. The registry lock joins
        // simultaneous selections until this lease publishes its chosen address.
        crate::store::with_locked_environments(&self.env, &self.cwd, |metas| {
            if source_watch_cancelled(lease, stop)? {
                return Err("dev UI setup was cancelled".to_string());
            }
            if !metas.iter().any(|current| {
                lease
                    .session()
                    .is_some_and(|session| session.restore_target_matches(current))
            }) {
                return Err("dev environment changed before UI startup".to_string());
            }
            let mut claimed = Vec::new();
            for other in metas {
                if other.name == meta.name {
                    continue;
                }
                if let Some(session) = service.source_watch_session(&other.name)?
                    && !session.closed
                    && let Some(target) = session.ui.as_ref().and_then(|ui| ui.target.as_ref())
                {
                    claimed.push(target.port);
                }
            }
            let port = crate::store::choose_source_ui_port(metas, &claimed, &self.env)?;
            let target = DevUiTarget {
                port,
                gateway_url: gateway_url.clone(),
            };
            target.validate(meta.gateway_port.unwrap_or_default())?;
            lease.claim_ui_target(SourceUiTarget {
                port,
                gateway_url: gateway_url.clone(),
            })
        })?;
        Ok(())
    }

    pub(super) fn run_source_gateway_ui(
        &self,
        meta: &EnvMeta,
        source: &Path,
        lease: &mut SourceWatchLease,
        stop: &AtomicBool,
    ) -> SourceWatchResult<i32> {
        if source_watch_cancelled(lease, stop)? {
            return Ok(130);
        }
        let captured = lease
            .session()
            .and_then(|session| session.ui.as_ref())
            .and_then(|ui| ui.target.as_ref())
            .ok_or_else(|| "dev UI target was not captured".to_string())?;
        let target = DevUiTarget {
            port: captured.port,
            gateway_url: captured.gateway_url.clone(),
        };
        target.validate(meta.gateway_port.unwrap_or_default())?;
        let mut children: Vec<OwnedUiChild> = Vec::new();
        let mut override_token = None;
        let result = (|| {
            let args = vec![
                "gateway".to_string(),
                "run".to_string(),
                "--port".to_string(),
                meta.gateway_port.unwrap_or_default().to_string(),
            ];
            let mut gateway = source_watch_node_command(
                if lease.is_watching() {
                    "scripts/watch-node.mjs"
                } else {
                    "scripts/run-node.mjs"
                },
                &args,
            );
            gateway
                .stdin(Stdio::inherit())
                .env_clear()
                .envs(build_openclaw_dev_source_env(meta, &self.env, source))
                .current_dir(source);
            children.push(spawn_ui_child(
                gateway,
                DevUiChildRole::Gateway,
                true,
                UiChildOutput::Logs(open_source_watch_log_files(meta)?),
                lease,
            )?);
            children.push(spawn_ui_child(
                target.command(meta, source, &self.env),
                DevUiChildRole::Ui,
                false,
                UiChildOutput::Logs(open_source_watch_log_files(meta)?),
                lease,
            )?);
            let active = self
                .environment_service()
                .create_source_watch_override_with_lease(
                    CreateSourceWatchOverrideOptions {
                        env_name: meta.name.clone(),
                        repo_root: source.to_path_buf(),
                        endpoint: SourceWatchEndpoint {
                            env_root: meta.root.clone(),
                            gateway_port: meta.gateway_port.unwrap_or_default(),
                        },
                        watch_pid: children[0].identity.pid,
                    },
                    lease,
                )?;
            override_token = Some(active.token.clone());
            for child in &mut children {
                if source_watch_cancelled(lease, stop)? {
                    return Ok(130);
                }
                child.start()?;
            }
            self.stdout_line(format!("UI address: http://127.0.0.1:{}/", target.port));
            let began = std::time::Instant::now();
            let mut last_probe = began - Duration::from_secs(1);
            let mut attempts = 0;
            let mut handoff_started: Option<std::time::Instant> = None;
            let mut discarded = false;
            let mut delivered = false;
            let mut pending_reported = false;
            let mut handoff_failure = None;
            loop {
                if source_watch_cancelled(lease, stop)? {
                    return Ok(130);
                }
                let mut index = 0;
                while index < children.len() {
                    let Some(status) = children[index].poll()? else {
                        index += 1;
                        continue;
                    };
                    children[index].finish(lease)?;
                    let mut child = children.remove(index);
                    if child.role != DevUiChildRole::Command {
                        self.stderr_line(format!(
                            "Dev {:?} exited; stopping the remaining dev processes.",
                            child.role
                        ));
                        return Ok(source_watch_status_code(
                            &status,
                            stop.load(Ordering::SeqCst),
                        ));
                    }
                    let expired = handoff_started
                        .is_some_and(|started| started.elapsed() >= DASHBOARD_TIMEOUT);
                    handoff_started = None;
                    last_probe = std::time::Instant::now();
                    let output = child.output();
                    if std::mem::take(&mut discarded) || expired {
                        attempts = DASHBOARD_ATTEMPTS;
                        if !pending_reported {
                            self.report_initial_ui_pending("the native dashboard request exceeded 30 seconds; no browser link was delivered");
                            pending_reported = true;
                        }
                        continue;
                    }
                    let link = if status.success() {
                        output.and_then(|(stdout, _)| target.handoff_url(&stdout))
                    } else {
                        Err("the native dashboard did not produce a browser handoff".to_string())
                    };
                    match link {
                        Ok(link) => {
                            if source_watch_cancelled(lease, stop)? {
                                return Ok(130);
                            }
                            self.stdout_line(format!("UI: {link}"));
                            delivered = true;
                        }
                        Err(error) => handoff_failure = Some(error),
                    }
                }
                if handoff_started.is_some_and(|started| started.elapsed() >= DASHBOARD_TIMEOUT)
                    && !discarded
                {
                    discarded = true;
                    attempts = DASHBOARD_ATTEMPTS;
                    self.report_initial_ui_pending("the native dashboard request exceeded 30 seconds; its helper remains owned until completion");
                    pending_reported = true;
                }
                if !delivered
                    && handoff_started.is_none()
                    && attempts < DASHBOARD_ATTEMPTS
                    && last_probe.elapsed() >= Duration::from_secs(1)
                {
                    last_probe = std::time::Instant::now();
                    if target.documents_ready() {
                        attempts += 1;
                        let service = self.environment_service();
                        let current = service.get(&meta.name)?;
                        if !lease
                            .session()
                            .is_some_and(|session| session.restore_target_matches(&current))
                        {
                            return Err("dev environment changed before the browser handoff"
                                .to_string()
                                .into());
                        }
                        let current = service.source_watch_environment(current, &active)?;
                        match spawn_ui_child(
                            target.dashboard_command(&current, source, &self.env),
                            DevUiChildRole::Command,
                            false,
                            UiChildOutput::Capture,
                            lease,
                        ) {
                            Ok(child) => {
                                children.push(child);
                                let child =
                                    children.last_mut().expect("dashboard child was recorded");
                                child.start()?;
                                handoff_started = child.started;
                                discarded = false;
                            }
                            Err(error) if error.cleanup_verified => {
                                handoff_failure = Some(error.message)
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
                if !delivered
                    && !pending_reported
                    && ((attempts == DASHBOARD_ATTEMPTS && handoff_started.is_none())
                        || began.elapsed() >= Duration::from_secs(10))
                {
                    self.report_initial_ui_pending(handoff_failure.as_deref().unwrap_or(
                        "waiting for the Gateway document, Vite and the native browser handoff",
                    ));
                    pending_reported = true;
                }
                #[cfg(unix)]
                if let Some(gateway) = children
                    .iter()
                    .find(|child| child.role == DevUiChildRole::Gateway)
                    && observe_process(gateway.identity.pid)?.is_some_and(|process| process.stopped)
                {
                    gateway
                        .guard
                        .suspend_with_child(gateway.identity.pid, lease, stop)?;
                }
                thread::sleep(Duration::from_millis(50));
            }
        })();
        let mut verified = source_watch_allows_service_restore(&result);
        let mut errors = Vec::new();
        // Persistent components precede Command, whose stop grace is limited to
        // the remaining original request budget. Every error still stops siblings.
        for child in &mut children {
            if let Err(error) = child.stop().and_then(|()| child.finish(lease)) {
                verified &= error.cleanup_verified;
                errors.push(error.message);
            }
        }
        if verified
            && let Some(token) = override_token
            && let Err(error) = self
                .environment_service()
                .clear_source_watch_override(&meta.name, &token)
        {
            errors.push(error);
        }
        match result {
            Ok(code) if errors.is_empty() => Ok(code),
            Ok(_) => Err(SourceWatchError {
                message: errors.join("; "),
                cleanup_verified: verified,
            }),
            Err(error) => {
                errors.insert(0, error.message);
                Err(SourceWatchError {
                    message: errors.join("; "),
                    cleanup_verified: verified,
                })
            }
        }
    }

    fn report_initial_ui_pending(&self, reason: &str) {
        self.stderr_line(format!(
            "UI link pending: {reason}. Gateway and Vite remain running."
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn handoff(target: &DevUiTarget) -> Value {
        let mut browser = Url::parse(&target.gateway_url).unwrap();
        let mut gateway = browser.clone();
        gateway.set_scheme("ws").unwrap();
        let fragment = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("bootstrapToken", "synthetic-owner-grant")
            .append_pair("bootstrapProfile", "control-ui-owner")
            .append_pair("gatewayUrl", gateway.as_str())
            .finish();
        browser.set_fragment(Some(&fragment));
        json!({
            "ok": true,
            "browserUrl": browser.as_str(),
            "browserBootstrapExpiresAtMs": (time::OffsetDateTime::now_utc().unix_timestamp() + 60) * 1000,
            "url": "http://127.0.0.1:18789/#token=synthetic-legacy-token",
            "gatewayPassword": "synthetic-legacy-password",
        })
    }

    #[test]
    fn dev_ui_handoff_retains_native_auth_and_logical_gateway_for_each_mount() {
        for base in ["/", "/console/", "/nested/control-ui/"] {
            let target = DevUiTarget {
                port: 5173,
                gateway_url: format!("http://127.0.0.1:18789{base}"),
            };
            let payload = handoff(&target);
            let before = Url::parse(payload["browserUrl"].as_str().unwrap()).unwrap();
            let output = target
                .handoff_url(&serde_json::to_vec(&payload).unwrap())
                .unwrap();
            let after = Url::parse(&output).unwrap();
            assert_eq!(
                after.origin().ascii_serialization(),
                "http://127.0.0.1:5173"
            );
            assert_eq!(after.path(), "/");
            assert_eq!(after.fragment(), before.fragment());
            assert!(!output.contains("synthetic-legacy"));
        }
    }

    #[test]
    fn dev_ui_handoff_rejects_foreign_or_ambiguous_auth_without_echoing_it() {
        let target = DevUiTarget {
            port: 5173,
            gateway_url: "http://127.0.0.1:18789/console/".to_string(),
        };
        let valid = handoff(&target);
        let valid_link = valid["browserUrl"].as_str().unwrap();
        let mut cases = vec![json!({"ok": false})];
        for (from, to) in [
            ("127.0.0.1:18789", "foreign.example:18789"),
            ("127.0.0.1:18789", "127.0.0.1:18790"),
            ("/console/#", "/other/#"),
            ("http://", "http://synthetic-private@"),
            ("/console/#", "/console/?secret=synthetic-private#"),
            ("bootstrapToken=", "unrecognizedToken="),
            ("bootstrapToken=synthetic-owner-grant", "bootstrapToken="),
            ("ws%3A", "wss%3A"),
            ("%2Fconsole%2F", "%2Fforeign%2F"),
        ] {
            assert!(valid_link.contains(from));
            let mut payload = valid.clone();
            payload["browserUrl"] = valid_link.replacen(from, to, 1).into();
            cases.push(payload);
        }
        for field in [
            "bootstrapToken=synthetic-private",
            "gatewayUrl=ws%3A%2F%2Fforeign.example",
        ] {
            let mut payload = valid.clone();
            payload["browserUrl"] = format!("{valid_link}&{field}").into();
            cases.push(payload);
        }
        for expires in [Value::Null, json!(0)] {
            let mut payload = valid.clone();
            payload["browserBootstrapExpiresAtMs"] = expires;
            cases.push(payload);
        }
        for payload in cases {
            let error = target
                .handoff_url(&serde_json::to_vec(&payload).unwrap())
                .unwrap_err();
            assert!(!error.contains("synthetic-"), "{error}");
        }
        assert!(
            target
                .handoff_url(b"synthetic-private malformed JSON")
                .is_err()
        );
    }
}
