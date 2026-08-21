use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::{Host, Url};

use crate::store::{list_environments, resolve_store_paths};

const INTERNAL_TAILSCALE_BIN_ENV: &str = "OCM_INTERNAL_TAILSCALE_BIN";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TailscaleCommandEndpoint {
    pub binary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
}

impl TailscaleCommandEndpoint {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        if let Some(socket) = self.socket.as_deref() {
            command.arg(format!("--socket={socket}"));
        }
        command
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NamedServiceProxyRoute {
    pub endpoint: TailscaleCommandEndpoint,
    pub service_name: String,
    pub public_host: String,
    pub original_proxy: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NamedServiceIngress {
    pub routes: Vec<NamedServiceProxyRoute>,
    pub identity_endpoints: Vec<TailscaleCommandEndpoint>,
    pub same_host_ips: Vec<String>,
    pub tailnet_login: String,
}

pub(crate) fn named_service_routes_for_gateway(
    gateway_port: u32,
    configured_service_name: Option<&str>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<NamedServiceIngress>, String> {
    let explicitly_configured = env
        .get(INTERNAL_TAILSCALE_BIN_ENV)
        .is_some_and(|value| !value.trim().is_empty());
    let endpoints = discover_tailscale_endpoints(env, cwd)?;
    let mut routes = Vec::new();
    let mut identity_endpoints = Vec::new();
    let mut same_host_ips = BTreeSet::new();
    let mut tailnet_login: Option<String> = None;
    let mut resolved_sources = BTreeSet::new();
    for endpoint in endpoints {
        if resolved_sources.contains(&endpoint.socket) {
            continue;
        }
        let output = match endpoint
            .command()
            .args(["serve", "status", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "failed to inspect Tailscale Serve configuration with {}: {error}",
                    endpoint.binary
                ));
            }
        };
        if !output.status.success() {
            continue;
        }
        let value = match parse_tailscale_json(&output.stdout) {
            Ok(value) => value,
            Err(_) if !explicitly_configured => continue,
            Err(error) => return Err(error),
        };
        resolved_sources.insert(endpoint.socket.clone());
        let mut endpoint_routes = named_service_routes_from_status(
            &value,
            gateway_port,
            configured_service_name,
            &endpoint,
        );
        if endpoint_routes.is_empty() {
            continue;
        }
        let status = endpoint
            .command()
            .args(["status", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| {
                format!(
                    "failed to inspect Tailscale identity with {}: {error}",
                    endpoint.binary
                )
            })?;
        if !status.status.success() {
            return Err(format!(
                "failed to inspect Tailscale identity with {}: {}",
                endpoint.binary,
                command_failure_summary(&status.stdout, &status.stderr)
            ));
        }
        let status = match parse_tailscale_json(&status.stdout) {
            Ok(status) => status,
            Err(_) if !explicitly_configured => continue,
            Err(error) => return Err(error),
        };
        if let Some(ips) = status
            .get("Self")
            .and_then(Value::as_object)
            .and_then(|self_node| self_node.get("TailscaleIPs"))
            .and_then(Value::as_array)
        {
            for ip in ips {
                if let Some(ip) = ip.as_str().map(str::trim)
                    && ip.parse::<IpAddr>().is_ok()
                {
                    same_host_ips.insert(ip.to_string());
                }
            }
        }
        let endpoint_login = status
            .get("CurrentTailnet")
            .and_then(Value::as_object)
            .and_then(|tailnet| tailnet.get("Name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "Tailscale status did not report the current tailnet login needed for trusted-proxy allowlisting"
                    .to_string()
            })?
            .to_string();
        let login_is_known_user =
            status
                .get("User")
                .and_then(Value::as_object)
                .is_some_and(|users| {
                    users.values().any(|user| {
                        user.get("LoginName")
                            .and_then(Value::as_str)
                            .is_some_and(|login| login.eq_ignore_ascii_case(&endpoint_login))
                    })
                });
        if !login_is_known_user {
            return Err(format!(
                "Tailscale tailnet name \"{endpoint_login}\" is not a verified user login; OCM cannot safely choose a trusted-proxy allowlist identity automatically"
            ));
        }
        if let Some(existing) = tailnet_login.as_deref()
            && !existing.eq_ignore_ascii_case(&endpoint_login)
        {
            return Err(format!(
                "named Tailscale Service routes for gateway port {gateway_port} span different tailnets ({existing} and {endpoint_login}); OCM cannot safely choose one identity boundary"
            ));
        }
        tailnet_login.get_or_insert(endpoint_login);
        routes.append(&mut endpoint_routes);
        identity_endpoints.push(endpoint);
    }
    if routes.is_empty() {
        return Ok(None);
    }
    routes.sort_by(|left, right| {
        (
            &left.endpoint.binary,
            &left.endpoint.socket,
            &left.service_name,
            &left.public_host,
        )
            .cmp(&(
                &right.endpoint.binary,
                &right.endpoint.socket,
                &right.service_name,
                &right.public_host,
            ))
    });
    routes.dedup();
    identity_endpoints
        .sort_by(|left, right| (&left.binary, &left.socket).cmp(&(&right.binary, &right.socket)));
    identity_endpoints.dedup();
    Ok(Some(NamedServiceIngress {
        routes,
        identity_endpoints,
        same_host_ips: same_host_ips.into_iter().collect(),
        tailnet_login: tailnet_login.expect("routes require a tailnet login"),
    }))
}

fn discover_tailscale_endpoints(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<TailscaleCommandEndpoint>, String> {
    let configured_binary = env
        .get(INTERNAL_TAILSCALE_BIN_ENV)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty());
    let default_binaries = configured_binary.map_or_else(
        || {
            [
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
                "/opt/homebrew/bin/tailscale",
                "/usr/local/bin/tailscale",
                "tailscale",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        },
        |binary| vec![binary.to_string()],
    );
    let mut endpoints = Vec::new();
    let mut seen = BTreeSet::new();
    for binary in &default_binaries {
        if seen.insert((binary.clone(), None)) {
            endpoints.push(TailscaleCommandEndpoint {
                binary: binary.clone(),
                socket: None,
            });
        }
    }
    let stores = resolve_store_paths(env, cwd)?;
    let socket_binaries = configured_binary.map_or_else(
        || {
            [
                "/opt/homebrew/bin/tailscale",
                "/usr/local/bin/tailscale",
                "tailscale",
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        },
        |binary| vec![binary.to_string()],
    );
    if stores.envs_dir.exists() {
        for env_meta in list_environments(env, cwd)? {
            let socket = Path::new(&env_meta.root)
                .join(".openclaw")
                .join("tailscale")
                .join("tailscaled.sock");
            if !socket.exists() {
                continue;
            }
            for binary in &socket_binaries {
                let socket = socket.to_string_lossy().into_owned();
                if seen.insert((binary.clone(), Some(socket.clone()))) {
                    endpoints.push(TailscaleCommandEndpoint {
                        binary: binary.clone(),
                        socket: Some(socket),
                    });
                }
            }
        }
    }
    Ok(endpoints)
}

fn parse_tailscale_json(stdout: &[u8]) -> Result<Value, String> {
    let start = stdout
        .iter()
        .position(|byte| *byte == b'{')
        .ok_or_else(|| "Tailscale Serve status did not return JSON".to_string())?;
    serde_json::from_slice(&stdout[start..])
        .map_err(|error| format!("failed to parse Tailscale Serve status JSON: {error}"))
}

fn named_service_routes_from_status(
    status: &Value,
    gateway_port: u32,
    configured_service_name: Option<&str>,
    endpoint: &TailscaleCommandEndpoint,
) -> Vec<NamedServiceProxyRoute> {
    let Some(services) = status.get("Services").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut routes = Vec::new();
    for (service_name, service) in services {
        let Some(web) = service.get("Web").and_then(Value::as_object) else {
            continue;
        };
        for (public_host, web_entry) in web {
            let Some(handlers) = web_entry.get("Handlers").and_then(Value::as_object) else {
                continue;
            };
            for handler in handlers.values() {
                let Some(proxy) = handler.get("Proxy").and_then(Value::as_str) else {
                    continue;
                };
                let configured_service_matches = configured_service_name
                    .is_some_and(|configured| configured.eq_ignore_ascii_case(service_name));
                if !(is_loopback_target(proxy, gateway_port)
                    || configured_service_matches && is_any_loopback_target(proxy))
                {
                    continue;
                }
                routes.push(NamedServiceProxyRoute {
                    endpoint: endpoint.clone(),
                    service_name: service_name.clone(),
                    public_host: public_host.clone(),
                    original_proxy: proxy.to_string(),
                });
            }
        }
    }
    routes.sort_by(|left, right| {
        (&left.service_name, &left.public_host).cmp(&(&right.service_name, &right.public_host))
    });
    routes.dedup();
    routes
}

fn is_loopback_target(proxy: &str, gateway_port: u32) -> bool {
    let Ok(url) = Url::parse(proxy) else {
        return false;
    };
    let Ok(gateway_port) = u16::try_from(gateway_port) else {
        return false;
    };
    if url.port_or_known_default() != Some(gateway_port) {
        return false;
    }
    is_loopback_url(&url)
}

fn is_any_loopback_target(proxy: &str) -> bool {
    Url::parse(proxy).is_ok_and(|url| is_loopback_url(&url))
}

fn is_loopback_url(url: &Url) -> bool {
    let Some(host) = url.host() else {
        return false;
    };
    match host {
        Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

#[derive(Debug)]
pub(crate) struct TailscaleServeConfigBackup {
    endpoint: TailscaleCommandEndpoint,
    service_name: String,
    original_proxy: String,
}

pub(crate) fn rewrite_named_service_routes(
    routes: &[NamedServiceProxyRoute],
    proxy_port: u16,
) -> Result<Vec<TailscaleServeConfigBackup>, String> {
    let mut grouped: BTreeMap<(TailscaleCommandEndpoint, String), &NamedServiceProxyRoute> =
        BTreeMap::new();
    for route in routes {
        grouped
            .entry((route.endpoint.clone(), route.service_name.clone()))
            .or_insert(route);
    }
    let mut backups = Vec::new();
    for ((endpoint, service_name), route) in grouped {
        set_service_proxy(
            &endpoint,
            &service_name,
            &format!("http://127.0.0.1:{proxy_port}"),
        )?;
        backups.push(TailscaleServeConfigBackup {
            endpoint,
            service_name,
            original_proxy: route.original_proxy.clone(),
        });
    }
    Ok(backups)
}

pub(crate) fn restore_named_service_routes(
    backups: &[TailscaleServeConfigBackup],
) -> Result<(), String> {
    for backup in backups.iter().rev() {
        set_service_proxy(
            &backup.endpoint,
            &backup.service_name,
            &backup.original_proxy,
        )?;
    }
    Ok(())
}

fn set_service_proxy(
    endpoint: &TailscaleCommandEndpoint,
    service_name: &str,
    target: &str,
) -> Result<(), String> {
    let output = endpoint
        .command()
        .args([
            "serve",
            &format!("--service={service_name}"),
            "--yes",
            target,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("failed to update Tailscale Serve route: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "failed to update Tailscale Serve route: {}",
            command_failure_summary(&output.stdout, &output.stderr)
        ))
    }
}

pub(crate) fn summarize_named_service_routes(routes: &[NamedServiceProxyRoute]) -> String {
    let mut services = BTreeSet::new();
    for route in routes {
        services.insert(format!("{} ({})", route.service_name, route.public_host));
    }
    services.into_iter().collect::<Vec<_>>().join(", ")
}

fn command_failure_summary(stdout: &[u8], stderr: &[u8]) -> String {
    for bytes in [stderr, stdout] {
        let value = String::from_utf8_lossy(bytes).trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    "command exited unsuccessfully".to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{TailscaleCommandEndpoint, named_service_routes_from_status};

    fn endpoint() -> TailscaleCommandEndpoint {
        TailscaleCommandEndpoint {
            binary: "tailscale".to_string(),
            socket: None,
        }
    }

    #[test]
    fn named_service_routes_match_only_loopback_handlers_for_the_gateway_port() {
        let status = json!({
            "Web": {
                "device.tailnet.ts.net:443": {
                    "Handlers": {
                        "/": {"Proxy": "http://127.0.0.1:19234"}
                    }
                }
            },
            "Services": {
                "svc:personal": {
                    "Web": {
                        "personal.tailnet.ts.net:443": {
                            "Handlers": {
                                "/": {"Proxy": "http://127.0.0.1:19234"},
                                "/other": {"Proxy": "http://127.0.0.1:19999"}
                            }
                        }
                    }
                },
                "svc:remote": {
                    "Web": {
                        "remote.tailnet.ts.net:443": {
                            "Handlers": {
                                "/": {"Proxy": "http://10.0.0.8:19234"}
                            }
                        }
                    }
                }
            }
        });

        let routes = named_service_routes_from_status(&status, 19234, None, &endpoint());
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].service_name, "svc:personal");
        assert_eq!(routes[0].public_host, "personal.tailnet.ts.net:443");
        assert_eq!(routes[0].original_proxy, "http://127.0.0.1:19234");
    }

    #[test]
    fn named_service_routes_cover_localhost_and_ipv6_loopback_targets() {
        let status = json!({
            "Services": {
                "svc:localhost": {
                    "Web": {
                        "localhost.tailnet.ts.net:443": {
                            "Handlers": {
                                "/": {"Proxy": "http://localhost:19234"}
                            }
                        }
                    }
                },
                "svc:ipv6": {
                    "Web": {
                        "ipv6.tailnet.ts.net:443": {
                            "Handlers": {
                                "/": {"Proxy": "http://[::1]:19234"}
                            }
                        }
                    }
                }
            }
        });

        let routes = named_service_routes_from_status(&status, 19234, None, &endpoint());
        assert_eq!(routes.len(), 2);
        let ipv6 = routes
            .iter()
            .find(|route| route.service_name == "svc:ipv6")
            .unwrap();
        let localhost = routes
            .iter()
            .find(|route| route.service_name == "svc:localhost")
            .unwrap();
        assert_eq!(ipv6.original_proxy, "http://[::1]:19234");
        assert_eq!(localhost.original_proxy, "http://localhost:19234");
    }

    #[test]
    fn configured_named_service_adopts_an_existing_loopback_proxy() {
        let status = json!({
            "Services": {
                "svc:main": {
                    "Web": {
                        "main.tailnet.ts.net:443": {
                            "Handlers": {
                                "/": {"Proxy": "http://127.0.0.1:20903"}
                            }
                        }
                    }
                },
                "svc:other": {
                    "Web": {
                        "other.tailnet.ts.net:443": {
                            "Handlers": {
                                "/": {"Proxy": "http://127.0.0.1:20904"}
                            }
                        }
                    }
                }
            }
        });

        let routes =
            named_service_routes_from_status(&status, 19123, Some("svc:main"), &endpoint());
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].service_name, "svc:main");
        assert_eq!(routes[0].original_proxy, "http://127.0.0.1:20903");
    }
}
