use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::inspect::{ServiceSummary, service_readiness_observation};
use crate::env::{EnvMeta, EnvironmentService, ExecutionBinding, resolve_execution_binding};
use crate::store::{GatewayReadinessHost, gateway_readiness_certificate, gateway_readiness_host};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

const DEFAULT_GATEWAY_READINESS_TIMEOUT: Duration = Duration::from_secs(90);
const GATEWAY_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(500);
const GATEWAY_READINESS_PROBE_BUDGET: Duration = Duration::from_millis(2100);

pub(crate) struct GatewayReadiness {
    pub(crate) ready: bool,
    pub(crate) status: ServiceSummary,
    pub(crate) issue: Option<String>,
    pub(crate) process_observed_after_ms: Option<u64>,
    pub(crate) ready_after_ms: Option<u64>,
}

pub(crate) fn wait_for_gateway_readiness(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<GatewayReadiness, String> {
    let timeout = gateway_readiness_timeout(env);
    wait_for_gateway_readiness_with_timeout(name, timeout, env, cwd)
}

pub(crate) fn gateway_readiness_timeout(env: &BTreeMap<String, String>) -> Duration {
    env.get("OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_GATEWAY_READINESS_TIMEOUT)
}

fn wait_for_gateway_readiness_with_timeout(
    name: &str,
    timeout: Duration,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<GatewayReadiness, String> {
    let interrupt_generation = crate::infra::interrupt::generation();
    let mut meta = EnvironmentService::new(env, cwd).get(name)?;
    let started = Instant::now();
    let deadline = Instant::now() + timeout;
    let mut native_gateway_config: Option<Result<Option<serde_json::Value>, String>> = None;
    let mut native_child_context = None;
    // A later incomplete observation must never turn a known TLS wait into an
    // unauthenticated HTTP probe, even if the config changes while starting.
    let mut known_tls = false;
    let mut permitted_retry_count = None;
    let mut process_observed_after_ms = None;
    loop {
        let (status, child_spec) = service_readiness_observation(name, env, cwd)?;
        if process_observed_after_ms.is_none() && status.child_pid.is_some() {
            process_observed_after_ms = Some(duration_ms(started.elapsed()));
        }
        let port = status.child_port.unwrap_or(status.gateway_port);
        let probe = if status.running {
            // One scheduled replacement may be observed after the main wait
            // deadline. Configuration and IO share that final finite budget;
            // the network probe must not restart it after a slow native lookup.
            let now = Instant::now();
            let observation_deadline = if deadline > now {
                deadline
            } else {
                now + GATEWAY_READINESS_PROBE_BUDGET
            };
            child_spec
                .as_ref()
                .ok_or_else(|| "Gateway launch environment is unavailable".to_string())
                .and_then(|child| {
                    let same_child = native_child_context.as_ref().is_some_and(
                        |(daemon, pid, restarts, spec)| {
                            *daemon == status.ocm_service_pid
                                && *pid == status.child_pid
                                && *restarts == status.child_restart_count
                                && spec == child
                        },
                    );
                    if !same_child {
                        // A scheduled retry can use a new desired spec. Resolve
                        // at most once for that child/context, retaining TLS
                        // evidence across the entire readiness wait.
                        let current = EnvironmentService::new(env, cwd).get(name)?;
                        let binding_matches = match resolve_execution_binding(&current, None, None)? {
                            ExecutionBinding::Dev => child.binding_kind == "dev",
                            ExecutionBinding::Runtime(binding) => {
                                child.binding_kind == "runtime" && child.binding_name == binding
                            }
                            ExecutionBinding::Launcher(binding) => {
                                child.binding_kind == "launcher" && child.binding_name == binding
                            }
                        };
                        if !binding_matches {
                            return Err("Gateway execution binding is changing; waiting for its supervised launch context".to_string());
                        }
                        meta = current;
                        native_gateway_config = None;
                        native_child_context = Some((
                            status.ocm_service_pid,
                            status.child_pid,
                            status.child_restart_count,
                            child.clone(),
                        ));
                    }
                    // A failed native observation remains a failure for this
                    // child even if a later local read no longer needs it.
                    if let Some(Err(error)) = &native_gateway_config {
                        return Err(error.clone());
                    }
                    let mut resolve_native = || {
                        native_gateway_config
                            .get_or_insert_with(|| {
                                resolve_native_gateway_config(
                                    &meta,
                                    Path::new(&child.run_dir),
                                    &child.process_env,
                                    cwd,
                                    observation_deadline,
                                    interrupt_generation,
                                )
                            })
                            .clone()
                    };
                    let certificate = gateway_readiness_certificate(
                        &meta,
                        port,
                        Path::new(&child.run_dir),
                        &child.process_env,
                        &mut known_tls,
                        &mut resolve_native,
                    )?;
                    let host = gateway_readiness_host(
                        &meta,
                        port,
                        Path::new(&child.run_dir),
                        &child.process_env,
                        &mut resolve_native,
                    )?;
                    gateway_bind_health_ok(host, port, certificate, observation_deadline)
                })
        } else {
            Ok(false)
        };
        if crate::infra::interrupt::interrupted_since(interrupt_generation) {
            return Err("Gateway readiness interrupted".to_string());
        }
        if matches!(probe, Ok(true)) {
            return Ok(GatewayReadiness {
                ready: true,
                status,
                issue: None,
                process_observed_after_ms,
                ready_after_ms: Some(duration_ms(started.elapsed())),
            });
        }

        if matches!(
            status.gateway_state.as_str(),
            "backoff" | "stopped" | "unknown"
        ) {
            if status.gateway_state == "backoff"
                && should_wait_for_scheduled_gateway_retry(
                    &mut permitted_retry_count,
                    status.child_restart_count,
                    status.next_retry_at.as_deref(),
                    time::OffsetDateTime::now_utc(),
                )
            {
                sleep(GATEWAY_READINESS_POLL_INTERVAL);
                continue;
            }
            let issue = status
                .issue
                .clone()
                .or_else(|| status.last_error.clone())
                .unwrap_or_else(|| {
                    format!(
                        "gateway exited with code {}",
                        status.last_exit_code.unwrap_or_default()
                    )
                });
            return Ok(GatewayReadiness {
                ready: false,
                status,
                issue: Some(issue),
                process_observed_after_ms,
                ready_after_ms: None,
            });
        }

        if Instant::now() >= deadline {
            let latest = probe
                .err()
                .or_else(|| status.issue.clone())
                .unwrap_or_else(|| status.gateway_state.clone());
            return Ok(GatewayReadiness {
                ready: false,
                status,
                issue: Some(format!(
                    "gateway did not become ready within {} seconds; latest status: {latest}",
                    timeout.as_secs_f64()
                )),
                process_observed_after_ms,
                ready_after_ms: None,
            });
        }
        sleep(
            GATEWAY_READINESS_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn resolve_native_gateway_config(
    meta: &EnvMeta,
    run_dir: &Path,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    deadline: Instant,
    interrupt_generation: u64,
) -> Result<Option<serde_json::Value>, String> {
    let args = ["config", "get", "gateway", "--json"].map(str::to_string);
    let mut command = match resolve_execution_binding(meta, None, None)? {
        ExecutionBinding::Dev => {
            meta.dev
                .as_ref()
                .ok_or("missing dev binding")?
                .execution_source_root()?;
            // Same read-only config capability used for templated dev UI paths.
            let mut command = Command::new("node");
            command
                .arg("scripts/run-node.mjs")
                .args(&args)
                .env_clear()
                .envs(env);
            command
        }
        ExecutionBinding::Runtime(name) => {
            let runtime = crate::store::get_runtime_verified(&name, env, cwd)?;
            if !crate::runtime::is_openclaw_package_runtime(&runtime) {
                return Ok(None);
            }
            let launch = crate::runtime::resolve_runtime_launch(&runtime, &args, env, cwd, false)?;
            let mut command_env = env.clone();
            crate::managed_node::apply_path_prepend_to_environment(
                &mut command_env,
                launch.path_prepend.as_deref(),
            )?;
            let mut command = Command::new(launch.program);
            command.args(launch.args).env_clear().envs(command_env);
            command
        }
        // An arbitrary launcher/registered executable need not implement config
        // commands. Keep its existing literal/inherited/config.env path support.
        ExecutionBinding::Launcher(_) => return Ok(None),
    };
    command.current_dir(run_dir);
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(
            "Gateway readiness deadline expired before TLS configuration lookup".to_string(),
        );
    }
    let output = crate::supervisor::read_private_config_command(
        command,
        remaining.min(Duration::from_secs(15)),
        interrupt_generation,
    )?;
    let gateway: serde_json::Value = serde_json::from_slice(&output)
        .map_err(|_| "OpenClaw did not return a Gateway configuration object".to_string())?;
    if !gateway.is_object() {
        return Err("OpenClaw did not return a Gateway configuration object".to_string());
    }
    if let Some(tls) = gateway.get("tls") {
        if !tls.is_object()
            || tls.get("enabled").is_some_and(|value| !value.is_boolean())
            || tls.get("certPath").is_some_and(|value| !value.is_string())
        {
            return Err("OpenClaw returned invalid Gateway TLS settings".to_string());
        }
    }
    Ok(Some(gateway))
}

/// The configured public leaf is the local endpoint identity, as in OpenClaw's
/// pinned local client. The TLS backend still verifies proof of its private key.
#[derive(Debug)]
struct GatewayCertificateVerifier {
    certificate: Vec<u8>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for GatewayCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() != self.certificate {
            return Err(rustls::Error::General(
                "Gateway TLS certificate mismatch".to_string(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

// Recompute the remaining budget before every TLS record read/write. Per-socket
// timeouts alone would let a trickling handshake extend readiness indefinitely.
struct ProbeSocket {
    stream: TcpStream,
    deadline: Instant,
}

impl ProbeSocket {
    fn timeout(&self) -> io::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(remaining.min(Duration::from_millis(800)))
    }
}

impl Read for ProbeSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.timeout()?))?;
        self.stream.read(buf)
    }
}

impl Write for ProbeSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.timeout()?))?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

// Query interface assignments rather than testing bind(): nonlocal bind can be
// enabled by the OS. No hostname lookup or network request authorizes a target.
#[cfg(unix)]
fn visit_local_addresses(
    mut visit: impl FnMut(&CStr, u32, Option<Ipv4Addr>) -> bool,
) -> Result<(), String> {
    let mut addresses = std::mem::MaybeUninit::<*mut libc::ifaddrs>::uninit();
    // SAFETY: getifaddrs initializes the output slot on success; all pointers remain
    // valid until the single freeifaddrs below, including on a found address.
    unsafe {
        if libc::getifaddrs(addresses.as_mut_ptr()) != 0 {
            return Err("Cannot inspect local Gateway interface addresses".to_string());
        }
        let addresses = addresses.assume_init();
        let mut current = addresses;
        while !current.is_null() {
            let address = (*current).ifa_addr;
            if !address.is_null() && !(*current).ifa_name.is_null() {
                let family = (*address).sa_family as i32;
                if family == libc::AF_INET || family == libc::AF_INET6 {
                    let host = if family == libc::AF_INET {
                        let address = &*address.cast::<libc::sockaddr_in>();
                        Some(Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()))
                    } else {
                        None
                    };
                    if visit(
                        CStr::from_ptr((*current).ifa_name),
                        (*current).ifa_flags,
                        host,
                    ) {
                        break;
                    }
                }
            }
            current = (*current).ifa_next;
        }
        libc::freeifaddrs(addresses);
        Ok(())
    }
}

#[cfg(not(unix))]
fn visit_local_addresses(
    _visit: impl FnMut(&CStr, u32, Option<Ipv4Addr>) -> bool,
) -> Result<(), String> {
    Err("Local Gateway interface inspection is unavailable on this platform".to_string())
}

fn is_local_ipv4(host: Ipv4Addr) -> Result<bool, String> {
    let mut found = false;
    visit_local_addresses(|_, _, address| {
        found = address == Some(host);
        found
    })?;
    Ok(found)
}

// Object.values(os.networkInterfaces()) enumerates array-index names first in
// numeric order, then other names in insertion order. IPv6 entries can establish
// a name's insertion order even though only IPv4 is eligible for this listener.
fn interface_order(name: &CStr, insertion_order: usize) -> (bool, usize) {
    let index = name.to_str().ok().and_then(|name| {
        name.parse::<u32>()
            .ok()
            .filter(|index| *index < u32::MAX && index.to_string() == name)
    });
    match index {
        Some(index) => (false, index as usize),
        None => (true, insertion_order),
    }
}

fn tailnet_ipv4() -> Result<Option<Ipv4Addr>, String> {
    let mut interfaces = BTreeMap::new();
    let mut selected = None;
    visit_local_addresses(|name, flags, host| {
        if !is_external_interface(flags) {
            return false;
        }
        let next = interfaces.len();
        let order = *interfaces.entry(name.to_bytes().to_vec()).or_insert(next);
        if let Some(host) = host.filter(|host| is_tailnet_interface(*host, flags)) {
            let order = interface_order(name, order);
            if selected.is_none_or(|(previous, _)| order < previous) {
                selected = Some((order, host));
            }
        }
        false
    })?;
    Ok(selected.map(|(_, host)| host))
}

// Match Node's external interface inventory and OpenClaw's 100.64.0.0/10
// selection. An interface name or a bindable nonlocal address is not authority.
#[cfg(unix)]
fn is_external_interface(flags: u32) -> bool {
    let required = (libc::IFF_UP | libc::IFF_RUNNING) as u32;
    flags & required == required && flags & libc::IFF_LOOPBACK as u32 == 0
}

#[cfg(not(unix))]
fn is_external_interface(_flags: u32) -> bool {
    false
}

fn is_tailnet_interface(host: Ipv4Addr, flags: u32) -> bool {
    is_external_interface(flags) && host.octets()[0] == 100 && (64..128).contains(&host.octets()[1])
}

fn gateway_bind_health_ok(
    host: GatewayReadinessHost,
    port: u32,
    certificate: Option<Vec<u8>>,
    readiness_deadline: Instant,
) -> Result<bool, String> {
    let tailnet = match host {
        GatewayReadinessHost::Address(host) => {
            return gateway_health_ok(host, port, certificate, readiness_deadline);
        }
        GatewayReadinessHost::Tailnet => tailnet_ipv4()?,
    };
    // Native tailnet mode can retain a loopback fallback chosen before an
    // interface appeared; newer Gateways also keep a second loopback listener.
    // Preserve that existing local probe and share one IO budget with tailnet.
    let deadline = readiness_deadline.min(Instant::now() + GATEWAY_READINESS_PROBE_BUDGET);
    let loopback = gateway_health_ok(Ipv4Addr::LOCALHOST, port, certificate.clone(), deadline);
    if matches!(loopback, Ok(true)) {
        return loopback;
    }
    match tailnet {
        Some(host) => gateway_health_ok(host, port, certificate, deadline),
        None => loopback,
    }
}

fn gateway_health_ok(
    host: Ipv4Addr,
    port: u32,
    certificate: Option<Vec<u8>>,
    readiness_deadline: Instant,
) -> Result<bool, String> {
    if port == 0 || port > u16::MAX as u32 {
        return Ok(false);
    }
    if !host.is_loopback() && !is_local_ipv4(host)? {
        return Err("Gateway bind address is not assigned to a local interface".to_string());
    }
    let now = Instant::now();
    // The caller owns any late-observation grace. Bound IO within its remaining
    // budget rather than renewing an already-expired configuration deadline.
    let deadline = readiness_deadline.min(now + GATEWAY_READINESS_PROBE_BUDGET);
    let connect_timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_millis(500));
    if connect_timeout.is_zero() {
        return Ok(false);
    }
    let addr = SocketAddrV4::new(host, port as u16);
    let Ok(stream) = TcpStream::connect_timeout(&addr.into(), connect_timeout) else {
        return Ok(false);
    };
    let mut socket = ProbeSocket { stream, deadline };
    if let Some(certificate) = certificate {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(GatewayCertificateVerifier {
            certificate,
            algorithms: provider.signature_verification_algorithms,
        });
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let mut connection =
            rustls::ClientConnection::new(Arc::new(config), ServerName::IpAddress(host.into()))
                .map_err(|error| error.to_string())?;
        // Authenticate the handshake before submitting the health request.
        while connection.is_handshaking() {
            if connection.complete_io(&mut socket).is_err() {
                return Err("Gateway TLS handshake or certificate verification failed".to_string());
            }
        }
        Ok(health_response_ok(
            &mut rustls::Stream::new(&mut connection, &mut socket),
            host,
            port,
        ))
    } else {
        Ok(health_response_ok(&mut socket, host, port))
    }
}

fn health_response_ok(stream: &mut (impl Read + Write), host: Ipv4Addr, port: u32) -> bool {
    let request =
        format!("GET /health HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }
    // Status lines can cross TCP packets or TLS records. Read a bounded line,
    // rather than accepting a partial prefix or depending on packet boundaries.
    let mut response = Vec::new();
    for _ in 0..256 {
        let mut byte = [0];
        if stream.read_exact(&mut byte).is_err() {
            return false;
        }
        response.push(byte[0]);
        if response.ends_with(b"\r\n") {
            let mut fields = response.split(|byte| *byte == b' ');
            return matches!(fields.next(), Some(b"HTTP/1.1" | b"HTTP/1.0"))
                && fields.next() == Some(b"200".as_slice());
        }
    }
    false
}

fn should_wait_for_scheduled_gateway_retry(
    permitted_retry_count: &mut Option<usize>,
    restart_count: Option<usize>,
    next_retry_at: Option<&str>,
    now: time::OffsetDateTime,
) -> bool {
    let (Some(restart_count), Some(next_retry_at)) = (restart_count, next_retry_at) else {
        return false;
    };
    let Ok(next_retry_at) = time::OffsetDateTime::parse(
        next_retry_at,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return false;
    };
    if now > next_retry_at + time::Duration::seconds(3) {
        return false;
    }
    match permitted_retry_count {
        Some(permitted) => restart_count == *permitted,
        None => {
            *permitted_retry_count = Some(restart_count);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::should_wait_for_scheduled_gateway_retry;

    #[test]
    fn interface_order_matches_native_numeric_names() {
        use super::interface_order;
        use std::ffi::CString;
        for (name, expected) in [
            ("0", (false, 0)),
            ("1", (false, 1)),
            ("4294967294", (false, 4294967294)),
            ("01", (true, 7)),
            ("-1", (true, 7)),
            ("4294967295", (true, 7)),
            ("tailscale0", (true, 7)),
        ] {
            assert_eq!(interface_order(&CString::new(name).unwrap(), 7), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn tailnet_selection_requires_an_active_external_cgnat_assignment() {
        use super::is_tailnet_interface;
        use std::net::Ipv4Addr;
        let active = (libc::IFF_UP | libc::IFF_RUNNING) as u32;
        for host in ["100.64.0.0", "100.100.100.1", "100.127.255.255"] {
            let host = host.parse::<Ipv4Addr>().unwrap();
            assert!(is_tailnet_interface(host, active));
            assert!(!is_tailnet_interface(host, libc::IFF_UP as u32));
            assert!(!is_tailnet_interface(host, libc::IFF_RUNNING as u32));
            assert!(!is_tailnet_interface(
                host,
                active | libc::IFF_LOOPBACK as u32
            ));
        }
        for host in ["100.63.255.255", "100.128.0.0", "127.0.0.1", "192.0.2.1"] {
            assert!(!is_tailnet_interface(host.parse().unwrap(), active));
        }
    }

    #[test]
    fn scheduled_retry_allows_only_the_observed_next_attempt() {
        let now = time::OffsetDateTime::now_utc();
        let next_retry_at = (now + time::Duration::seconds(1))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let mut permitted_retry_count = None;

        assert!(should_wait_for_scheduled_gateway_retry(
            &mut permitted_retry_count,
            Some(2),
            Some(&next_retry_at),
            now,
        ));
        assert!(should_wait_for_scheduled_gateway_retry(
            &mut permitted_retry_count,
            Some(2),
            Some(&next_retry_at),
            now,
        ));
        assert!(!should_wait_for_scheduled_gateway_retry(
            &mut permitted_retry_count,
            Some(3),
            Some(&next_retry_at),
            now,
        ));
    }

    #[test]
    fn scheduled_retry_rejects_missing_stale_or_invalid_state() {
        let now = time::OffsetDateTime::now_utc();
        let stale_retry_at = (now - time::Duration::seconds(4))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();

        assert!(!should_wait_for_scheduled_gateway_retry(
            &mut None,
            Some(1),
            None,
            now,
        ));
        assert!(!should_wait_for_scheduled_gateway_retry(
            &mut None,
            Some(1),
            Some(&stale_retry_at),
            now,
        ));
        assert!(!should_wait_for_scheduled_gateway_retry(
            &mut None,
            Some(1),
            Some("not-a-timestamp"),
            now,
        ));
    }
}
