use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::inspect::{ServiceSummary, service_status_fast};

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
    let timeout = env
        .get("OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_GATEWAY_READINESS_TIMEOUT);
    wait_for_gateway_readiness_with_timeout(name, timeout, env, cwd)
}

fn wait_for_gateway_readiness_with_timeout(
    name: &str,
    timeout: Duration,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<GatewayReadiness, String> {
    let started = Instant::now();
    let deadline = Instant::now() + timeout;
    let mut permitted_retry_count = None;
    let mut process_observed_after_ms = None;
    loop {
        let status = service_status_fast(name, env, cwd)?;
        if process_observed_after_ms.is_none() && status.child_pid.is_some() {
            process_observed_after_ms = Some(duration_ms(started.elapsed()));
        }
        if status.running && gateway_health_ok(status.child_port.unwrap_or(status.gateway_port)) {
            return Ok(GatewayReadiness {
                ready: true,
                status,
                issue: None,
                process_observed_after_ms,
                ready_after_ms: Some(duration_ms(started.elapsed())),
            });
        }

        if matches!(status.gateway_state.as_str(), "backoff" | "stopped") {
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
            let latest = status
                .issue
                .clone()
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

fn gateway_health_ok(port: u32) -> bool {
    if port == 0 || port > u16::MAX as u32 {
        return false;
    }
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port as u16);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr.into(), Duration::from_millis(500))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
    let request =
        format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = [0_u8; 256];
    let Ok(read) = stream.read(&mut response) else {
        return false;
    };
    let text = String::from_utf8_lossy(&response[..read]);
    text.starts_with("HTTP/1.1 200") || text.starts_with("HTTP/1.0 200")
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
