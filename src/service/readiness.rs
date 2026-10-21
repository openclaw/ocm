use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::inspect::{ServiceSummary, service_status_for_readiness};
use crate::env::{EnvMeta, EnvironmentService, ExecutionBinding, resolve_execution_binding};
use crate::infra::shell::{build_openclaw_dev_source_env, build_openclaw_env};
use crate::store::gateway_readiness_certificate;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

const DEFAULT_GATEWAY_READINESS_TIMEOUT: Duration = Duration::from_secs(90);
const GATEWAY_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(500);

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
    let meta = EnvironmentService::new(env, cwd).get(name)?;
    let started = Instant::now();
    let deadline = Instant::now() + timeout;
    let mut native_certificate_path = None;
    let mut permitted_retry_count = None;
    let mut process_observed_after_ms = None;
    loop {
        let status = service_status_for_readiness(name, env, cwd)?;
        if process_observed_after_ms.is_none() && status.child_pid.is_some() {
            process_observed_after_ms = Some(duration_ms(started.elapsed()));
        }
        let port = status.child_port.unwrap_or(status.gateway_port);
        let probe = if status.running {
            gateway_readiness_certificate(&meta, port, Path::new(&status.run_dir), env, || {
                native_certificate_path
                    .get_or_insert_with(|| {
                        resolve_native_certificate_path(
                            &meta,
                            port,
                            Path::new(&status.run_dir),
                            env,
                            cwd,
                            deadline,
                        )
                    })
                    .clone()
            })
            .and_then(|certificate| gateway_health_ok(port, certificate, deadline))
        } else {
            Ok(false)
        };
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

fn resolve_native_certificate_path(
    meta: &EnvMeta,
    port: u32,
    run_dir: &Path,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    deadline: Instant,
) -> Result<Option<String>, String> {
    let args = ["config", "get", "gateway.tls.certPath", "--json"].map(str::to_string);
    let mut meta = meta.clone();
    meta.gateway_port = Some(port);
    let mut command = match resolve_execution_binding(&meta, None, None)? {
        ExecutionBinding::Dev => {
            let source = meta
                .dev
                .as_ref()
                .ok_or("missing dev binding")?
                .execution_source_root()?;
            // Same read-only config capability used for templated dev UI paths.
            let mut command = Command::new("node");
            command
                .arg("scripts/run-node.mjs")
                .args(&args)
                .env_clear()
                .envs(build_openclaw_dev_source_env(&meta, env, source));
            command
        }
        ExecutionBinding::Runtime(name) => {
            let runtime = crate::store::get_runtime_verified(&name, env, cwd)?;
            if !crate::runtime::is_openclaw_package_runtime(&runtime) {
                return Ok(None);
            }
            let launch = crate::runtime::resolve_runtime_launch(&runtime, &args, env, cwd, false)?;
            let mut command_env = build_openclaw_env(&meta, env);
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
    )?;
    let path: String = serde_json::from_slice(&output)
        .map_err(|_| "OpenClaw did not return a string for gateway.tls.certPath".to_string())?;
    Ok(Some(path))
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

fn gateway_health_ok(
    port: u32,
    certificate: Option<Vec<u8>>,
    readiness_deadline: Instant,
) -> Result<bool, String> {
    if port == 0 || port > u16::MAX as u32 {
        return Ok(false);
    }
    let now = Instant::now();
    // The wait loop permits one scheduled restart attempt even after its main
    // deadline. Give that final observation the same finite probe budget.
    let deadline = if readiness_deadline > now {
        readiness_deadline.min(now + Duration::from_millis(2100))
    } else {
        now + Duration::from_millis(2100)
    };
    let connect_timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_millis(500));
    if connect_timeout.is_zero() {
        return Ok(false);
    }
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port as u16);
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
        let mut connection = rustls::ClientConnection::new(
            Arc::new(config),
            ServerName::IpAddress(Ipv4Addr::LOCALHOST.into()),
        )
        .map_err(|error| error.to_string())?;
        // Authenticate the handshake before submitting the health request.
        while connection.is_handshaking() {
            if connection.complete_io(&mut socket).is_err() {
                return Err("Gateway TLS handshake or certificate verification failed".to_string());
            }
        }
        Ok(health_response_ok(
            &mut rustls::Stream::new(&mut connection, &mut socket),
            port,
        ))
    } else {
        Ok(health_response_ok(&mut socket, port))
    }
}

fn health_response_ok(stream: &mut (impl Read + Write), port: u32) -> bool {
    let request =
        format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
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
