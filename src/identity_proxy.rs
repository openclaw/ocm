use std::collections::BTreeMap;
use std::fs;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::env::EnvironmentService;
use crate::service::inspect::{LaunchdJobStatus, inspect_job};
use crate::service::platform::{
    ManagedServiceDefinition, activate_managed_service, deactivate_managed_service,
    service_definition_dir, service_definition_extension, service_manager_kind,
    write_managed_service_definition,
};
use crate::store::{display_path, ensure_dir, resolve_store_paths, write_json};
use crate::tailscale::TailscaleCommandEndpoint;

const IDENTITY_PROXY_SCRIPT: &str = include_str!("identity_proxy.mjs");
const IDENTITY_PROXY_PORT_OFFSET: u32 = 2_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IdentityProxyConfig {
    pub env_name: String,
    pub listen_host: String,
    pub listen_port: u16,
    pub upstream_host: String,
    pub upstream_port: u16,
    pub tailscale_endpoints: Vec<TailscaleCommandEndpoint>,
    pub same_host_ips: Vec<String>,
    pub same_host_login: String,
}

#[derive(Clone, Debug)]
pub(crate) struct IdentityProxyPlan {
    pub config: IdentityProxyConfig,
}

#[derive(Debug)]
pub(crate) struct IdentityProxyRollback {
    definition: ManagedServiceDefinition,
    previous_definition: Option<Vec<u8>>,
    previous_config: Option<Vec<u8>>,
    previous_script: Option<Vec<u8>>,
    previous_status: LaunchdJobStatus,
    config_path: PathBuf,
    script_path: PathBuf,
}

impl IdentityProxyRollback {
    pub(crate) fn rollback(&self, env: &BTreeMap<String, String>) -> Result<(), String> {
        deactivate_managed_service(&self.definition, env)?;
        restore_file(&self.config_path, self.previous_config.as_deref())?;
        restore_file(&self.script_path, self.previous_script.as_deref())?;
        restore_file(
            &self.definition.definition_path,
            self.previous_definition.as_deref(),
        )?;
        if self.previous_status.loaded {
            activate_managed_service(
                &self.definition.label,
                &self.definition.definition_path,
                env,
            )?;
        }
        Ok(())
    }
}

pub(crate) fn plan_identity_proxy(
    env_name: &str,
    gateway_port: u32,
    tailscale_endpoints: Vec<TailscaleCommandEndpoint>,
    same_host_ips: Vec<String>,
    same_host_login: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<IdentityProxyPlan, String> {
    let listen_port = u16::try_from(
        gateway_port
            .checked_add(IDENTITY_PROXY_PORT_OFFSET)
            .ok_or_else(|| "gateway port cannot be mapped to an identity proxy port".to_string())?,
    )
    .map_err(|_| "gateway port cannot be mapped to an identity proxy port".to_string())?;
    let upstream_port =
        u16::try_from(gateway_port).map_err(|_| "gateway port is out of range".to_string())?;
    let (_, config_path, _) = identity_proxy_paths(env_name, env, cwd)?;
    if !config_path.exists() && TcpListener::bind((Ipv4Addr::LOCALHOST, listen_port)).is_err() {
        return Err(format!(
            "identity proxy port {listen_port} is already in use; free it or configure the named Tailscale Service explicitly before upgrading"
        ));
    }
    Ok(IdentityProxyPlan {
        config: IdentityProxyConfig {
            env_name: env_name.to_string(),
            listen_host: "127.0.0.1".to_string(),
            listen_port,
            upstream_host: "127.0.0.1".to_string(),
            upstream_port,
            tailscale_endpoints,
            same_host_ips,
            same_host_login: same_host_login.to_string(),
        },
    })
}

pub(crate) fn install_identity_proxy(
    plan: &IdentityProxyPlan,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<IdentityProxyRollback, String> {
    let (script_path, config_path, logs_dir) =
        identity_proxy_paths(&plan.config.env_name, env, cwd)?;
    ensure_dir(
        script_path
            .parent()
            .ok_or_else(|| "identity proxy script path has no parent".to_string())?,
    )?;
    ensure_dir(&logs_dir)?;
    let node = resolve_node_binary(&plan.config.env_name, env, cwd)?;
    let definition = identity_proxy_definition(
        &plan.config.env_name,
        &node,
        &script_path,
        &config_path,
        &logs_dir,
        env,
        cwd,
    )?;
    let previous_status = inspect_job(&definition.label, &definition.definition_path, env);
    let rollback = IdentityProxyRollback {
        previous_definition: fs::read(&definition.definition_path).ok(),
        previous_config: fs::read(&config_path).ok(),
        previous_script: fs::read(&script_path).ok(),
        previous_status,
        config_path: config_path.clone(),
        script_path: script_path.clone(),
        definition: definition.clone(),
    };
    if definition.definition_path.exists() {
        deactivate_managed_service(&definition, env)?;
    }
    fs::write(&script_path, IDENTITY_PROXY_SCRIPT)
        .map_err(|error| format!("failed to write identity proxy script: {error}"))?;
    write_json(&config_path, &plan.config)?;
    write_managed_service_definition(&definition, env)?;
    activate_managed_service(&definition.label, &definition.definition_path, env)?;
    if env
        .get("OCM_INTERNAL_IDENTITY_PROXY_SKIP_READY")
        .map(String::as_str)
        != Some("1")
    {
        wait_for_proxy(plan.config.listen_port)?;
    }
    Ok(rollback)
}

pub(crate) fn identity_proxy_trusted_proxies() -> Vec<String> {
    vec!["127.0.0.1/32".to_string()]
}

fn resolve_node_binary(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let process = EnvironmentService::new(env, cwd).resolve_gateway_process(env_name, false)?;
    if let Some(binary) = process.binary_path {
        let path = PathBuf::from(binary);
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "node" | "node.exe"))
            && path.is_file()
        {
            return Ok(path);
        }
    }
    let path = env.get("PATH").map(String::as_str).unwrap_or_default();
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(if cfg!(windows) { "node.exe" } else { "node" });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("failed to resolve the Node.js binary required by the OCM identity proxy".to_string())
}

fn identity_proxy_paths(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let root = resolve_store_paths(env, cwd)?
        .home
        .join("ingress")
        .join(env_name);
    Ok((
        root.join("identity-proxy.mjs"),
        root.join("identity-proxy.json"),
        root.join("logs"),
    ))
}

fn identity_proxy_definition(
    env_name: &str,
    node: &Path,
    script_path: &Path,
    config_path: &Path,
    logs_dir: &Path,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ManagedServiceDefinition, String> {
    let label = format!("ai.openclaw.ocm.ingress.{}", sanitize_label(env_name));
    let definition_path = service_definition_dir(env).join(format!(
        "{}.{}",
        label,
        service_definition_extension(service_manager_kind(env))
    ));
    let mut service_env = BTreeMap::new();
    service_env.insert(
        "OCM_HOME".to_string(),
        display_path(&resolve_store_paths(env, cwd)?.home),
    );
    Ok(ManagedServiceDefinition {
        label,
        description: format!("OCM Tailscale identity ingress for {env_name}"),
        definition_path,
        program_arguments: vec![
            display_path(node),
            display_path(script_path),
            display_path(config_path),
        ],
        working_directory: script_path
            .parent()
            .ok_or_else(|| "identity proxy script path has no parent".to_string())?
            .to_path_buf(),
        stdout_path: logs_dir.join("stdout.log"),
        stderr_path: logs_dir.join("stderr.log"),
        environment: service_env,
    })
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn wait_for_proxy(port: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect_timeout(
            &SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "identity proxy did not start listening on 127.0.0.1:{port}"
            ));
        }
        sleep(Duration::from_millis(100));
    }
}

fn restore_file(path: &Path, contents: Option<&[u8]>) -> Result<(), String> {
    match contents {
        Some(contents) => fs::write(path, contents).map_err(|error| error.to_string()),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    }
}
