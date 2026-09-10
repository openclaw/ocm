use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::TcpListener;
use std::path::Path;

use serde_json::Value;

use crate::env::EnvMeta;

use super::layout::{derive_env_paths, resolve_user_home};

pub(crate) const DEFAULT_GATEWAY_PORT: u32 = 18_789;
const OPENCLAW_PORT_FAMILY_END_OFFSET: u32 = 110;

pub(crate) fn resolve_env_gateway_port(meta: &EnvMeta) -> Option<u32> {
    meta.gateway_port
        .filter(|_| !meta.gateway_port_auto_assigned)
        .or_else(|| resolve_config_gateway_port(meta))
        .or(meta.gateway_port)
}

pub(crate) fn resolve_config_gateway_port(meta: &EnvMeta) -> Option<u32> {
    read_gateway_port_from_config(&derive_env_paths(Path::new(&meta.root)).config_path)
}

pub(crate) fn resolve_effective_gateway_ports(
    envs: &[EnvMeta],
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, u32> {
    let mut sorted = envs.to_vec();
    sorted.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.name.cmp(&right.name))
    });

    let mut claimed = BTreeSet::new();
    reserve_foreign_openclaw_port_family(env, &mut claimed);
    reserve_dev_ui_ports(envs, &mut claimed);

    let mut effective = BTreeMap::new();
    for meta in &sorted {
        if let Some(port) = resolve_env_gateway_port(meta) {
            reserve_openclaw_port_family(port, &mut claimed);
            effective.insert(meta.name.clone(), port);
        }
    }

    for meta in &sorted {
        if effective.contains_key(&meta.name) {
            continue;
        }

        let port = next_available_gateway_port(DEFAULT_GATEWAY_PORT, &claimed);
        reserve_openclaw_port_family(port, &mut claimed);
        effective.insert(meta.name.clone(), port);
    }

    effective
}

pub(crate) fn choose_available_gateway_port(
    preferred_port: u32,
    envs: &[EnvMeta],
    env: &BTreeMap<String, String>,
) -> u32 {
    let effective = resolve_effective_gateway_ports(envs, env);
    let mut claimed = BTreeSet::new();
    reserve_foreign_openclaw_port_family(env, &mut claimed);
    reserve_dev_ui_ports(envs, &mut claimed);
    for port in effective.values().copied() {
        reserve_openclaw_port_family(port, &mut claimed);
    }

    next_available_gateway_port(preferred_port.max(DEFAULT_GATEWAY_PORT), &claimed)
}

fn reserve_foreign_openclaw_port_family(
    env: &BTreeMap<String, String>,
    claimed: &mut BTreeSet<u32>,
) {
    let config_path = resolve_user_home(env)
        .join(".openclaw")
        .join("openclaw.json");
    if let Some(port) = read_gateway_port_from_config(&config_path) {
        reserve_openclaw_port_family(port, claimed);
    }
}

fn reserve_dev_ui_ports(envs: &[EnvMeta], claimed: &mut BTreeSet<u32>) {
    claimed.extend(envs.iter().filter_map(|meta| meta.dev_ui_port));
}

pub(super) fn choose_source_ui_port(
    meta: &EnvMeta,
    envs: &[EnvMeta],
    active_ui_ports: &[u32],
    env: &BTreeMap<String, String>,
) -> Result<u32, String> {
    let mut claimed = BTreeSet::new();
    reserve_foreign_openclaw_port_family(env, &mut claimed);
    for port in resolve_effective_gateway_ports(envs, env).values().copied() {
        reserve_openclaw_port_family(port, &mut claimed);
    }
    claimed.extend(active_ui_ports);
    claimed.extend(
        envs.iter()
            .filter(|other| other.name != meta.name)
            .filter_map(|other| other.dev_ui_port),
    );
    let available = |port: u32| {
        !claimed.contains(&port) && TcpListener::bind(("127.0.0.1", port as u16)).is_ok()
    };
    if let Some(port) = meta.dev_ui_port {
        if !(1..=u16::MAX as u32).contains(&port) {
            return Err(format!("env {} has an invalid retained UI port", meta.name));
        }
        if !available(port) {
            return Err(format!(
                "retained UI port {port} for env {} is occupied or reserved; release that port before retrying",
                meta.name,
            ));
        }
        return Ok(port);
    }
    (5173..=u16::MAX as u32)
        .find(|port| available(*port))
        .ok_or_else(|| "no unreserved loopback port is available for the dev UI".to_string())
}

fn next_available_gateway_port(start: u32, claimed: &BTreeSet<u32>) -> u32 {
    let mut port = start.max(DEFAULT_GATEWAY_PORT);
    while openclaw_port_family_conflicts(port, claimed) || !openclaw_port_family_available(port) {
        port = port.saturating_add(1);
    }
    port
}

fn openclaw_port_family_conflicts(base_port: u32, claimed: &BTreeSet<u32>) -> bool {
    openclaw_port_family(base_port)
        .into_iter()
        .any(|port| claimed.contains(&port))
}

pub(crate) fn openclaw_port_family_available(base_port: u32) -> bool {
    let mut listeners = Vec::new();
    for port in openclaw_port_family(base_port) {
        match TcpListener::bind(("127.0.0.1", port as u16)) {
            Ok(listener) => listeners.push(listener),
            Err(_) => return false,
        }
    }
    true
}

fn reserve_openclaw_port_family(base_port: u32, claimed: &mut BTreeSet<u32>) {
    for port in openclaw_port_family(base_port) {
        claimed.insert(port);
    }
}

pub(crate) fn openclaw_port_family_range(base_port: u32) -> (u32, u32) {
    let end_port = base_port
        .saturating_add(OPENCLAW_PORT_FAMILY_END_OFFSET)
        .min(u16::MAX as u32);
    (base_port, end_port)
}

fn openclaw_port_family(base_port: u32) -> Vec<u32> {
    let (start_port, end_port) = openclaw_port_family_range(base_port);
    (start_port..=end_port).collect()
}

fn read_gateway_port_from_config(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    read_port_number(value.get("gateway")?.get("port")?)
}

pub(super) fn read_port_number(value: &Value) -> Option<u32> {
    let number = value.as_number()?;
    if let Some(port) = number.as_u64() {
        return (1..=u16::MAX as u64).contains(&port).then_some(port as u32);
    }

    let port = number.as_f64()?;
    (port.fract() == 0.0 && (1.0..=u16::MAX as f64).contains(&port)).then_some(port as u32)
}
