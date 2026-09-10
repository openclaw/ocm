use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::env::EnvMeta;

const KIND: &str = "ocm-dev-ui-ports";
const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reservations {
    kind: String,
    ports: BTreeMap<String, Reservation>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reservation {
    env_root: String,
    #[serde(with = "time::serde::rfc3339")]
    env_created_at: OffsetDateTime,
    port: u32,
}

impl Reservation {
    fn matches(&self, meta: &EnvMeta) -> bool {
        self.env_root == meta.root && self.env_created_at == meta.created_at
    }
}

fn path(env: &BTreeMap<String, String>, cwd: &Path) -> Result<std::path::PathBuf, String> {
    Ok(super::resolve_store_paths(env, cwd)?
        .home
        .join("dev-ui-ports.json"))
}

fn load(env: &BTreeMap<String, String>, cwd: &Path) -> Result<Reservations, String> {
    let path = path(env, cwd)?;
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Reservations {
                kind: KIND.to_string(),
                ports: BTreeMap::new(),
            });
        }
        Err(error) => {
            return Err(format!(
                "failed reading UI port reservations {}: {error}",
                super::display_path(&path)
            ));
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("UI port reservations exceed their metadata size limit".to_string());
    }
    let saved: Reservations = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid UI port reservations: {error}"))?;
    if saved.kind != KIND || saved.ports.len() > u16::MAX as usize {
        return Err("unsupported UI port reservations".to_string());
    }
    for (name, reservation) in &saved.ports {
        if super::validate_name(name, "Environment name")? != *name
            || !Path::new(&reservation.env_root).is_absolute()
            || !(1..=u16::MAX as u32).contains(&reservation.port)
        {
            return Err("invalid UI port reservation identity or port".to_string());
        }
    }
    Ok(saved)
}

fn write(saved: &Reservations, env: &BTreeMap<String, String>, cwd: &Path) -> Result<(), String> {
    if saved.ports.len() > u16::MAX as usize {
        return Err("too many UI port reservations".to_string());
    }
    let mut bytes = serde_json::to_vec_pretty(saved).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_BYTES {
        return Err("UI port reservations exceed their metadata size limit".to_string());
    }
    super::common::write_file_replacing_path(&path(env, cwd)?, &bytes)
}

/// One bounded read supplies all views; older env-registry writers cannot erase
/// this owner's reservations when rewriting unrelated environment rows.
pub(super) fn hydrate(
    metas: &mut [EnvMeta],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let saved = load(env, cwd)?;
    for meta in metas {
        // Registry fields are views, not ownership. In particular, a copied
        // registry row must not carry an address into a new environment.
        meta.dev_ui_port = saved
            .ports
            .get(&meta.name)
            .filter(|reservation| reservation.matches(meta))
            .map(|reservation| reservation.port);
    }
    Ok(())
}

/// The caller holds the environment operation and registry locks.
pub(crate) fn reserve(
    meta: &EnvMeta,
    metas: &[EnvMeta],
    active_ui_ports: &[u32],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<u32, String> {
    let port = super::gateway_ports::choose_source_ui_port(meta, metas, active_ui_ports, env)?;
    let mut saved = load(env, cwd)?;
    let previous_count = saved.ports.len();
    let current: BTreeMap<_, _> = metas
        .iter()
        .map(|meta| (meta.name.as_str(), meta))
        .collect();
    saved.ports.retain(|name, reservation| {
        current
            .get(name.as_str())
            .is_some_and(|meta| reservation.matches(meta))
    });
    let reservation = Reservation {
        env_root: meta.root.clone(),
        env_created_at: meta.created_at,
        port,
    };
    if saved.ports.len() != previous_count || saved.ports.get(&meta.name) != Some(&reservation) {
        saved.ports.insert(meta.name.clone(), reservation);
        write(&saved, env, cwd)?;
    }
    Ok(port)
}

/// Remove only the reservation owned by the removed environment's identity.
pub(super) fn remove(
    meta: &EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let mut saved = load(env, cwd)?;
    if saved
        .ports
        .get(&meta.name)
        .is_some_and(|reservation| reservation.matches(meta))
    {
        saved.ports.remove(&meta.name);
        write(&saved, env, cwd)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::TcpListener;

    use crate::env::{
        CloneEnvironmentOptions, CreateEnvSnapshotOptions, CreateEnvironmentOptions,
        ExportEnvironmentOptions, ImportEnvironmentOptions, RestoreEnvSnapshotOptions,
    };
    use crate::store::{
        create_environment, get_environment, list_environments, remove_environment,
        save_environment,
    };

    use super::*;

    fn environment(root: &Path) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("HOME".to_string(), root.join("home").display().to_string()),
            (
                "OCM_HOME".to_string(),
                root.join("store").display().to_string(),
            ),
        ])
    }

    fn create(name: &str, env: &BTreeMap<String, String>, cwd: &Path) -> EnvMeta {
        create_environment(
            CreateEnvironmentOptions {
                name: name.to_string(),
                root: None,
                gateway_port: Some(21999),
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev: None,
                protected: false,
            },
            env,
            cwd,
        )
        .unwrap()
    }

    fn reserve_for(name: &str, env: &BTreeMap<String, String>, cwd: &Path) -> EnvMeta {
        reserve_for_result(name, env, cwd).unwrap()
    }

    fn erase_unaware_writer_fields(env: &BTreeMap<String, String>, cwd: &Path) {
        let registry = crate::store::env_registry_path(env, cwd).unwrap();
        let mut data: serde_json::Value = crate::store::read_json(&registry).unwrap();
        for meta in data["envs"].as_array_mut().unwrap() {
            meta.as_object_mut().unwrap().remove("devUiPort");
        }
        crate::store::write_json(&registry, &data).unwrap();
    }

    #[test]
    fn dev_ui_ports_survive_old_writers_restore_clone_and_removal() {
        let root = tempfile::tempdir().unwrap();
        let env = environment(root.path());
        let mut original = create("original", &env, root.path());
        let snapshot = crate::store::create_env_snapshot(
            CreateEnvSnapshotOptions {
                env_name: original.name.clone(),
                label: None,
            },
            &env,
            root.path(),
        )
        .unwrap();
        let socket = (30000..30100)
            .find_map(|port| TcpListener::bind(("127.0.0.1", port)).ok())
            .unwrap();
        let port = u32::from(socket.local_addr().unwrap().port());
        drop(socket);
        // Seed a high retained port so the Gateway allocation control would
        // overlap it without the shared reservation exclusion.
        let saved = Reservations {
            kind: KIND.to_string(),
            ports: BTreeMap::from([(
                original.name.clone(),
                Reservation {
                    env_root: original.root.clone(),
                    env_created_at: original.created_at,
                    port,
                },
            )]),
        };
        crate::store::write_json(&path(&env, root.path()).unwrap(), &saved).unwrap();
        original = reserve_for("original", &env, root.path());
        save_environment(original.clone(), &env, root.path()).unwrap();
        assert_eq!(original.dev_ui_port, Some(port));
        erase_unaware_writer_fields(&env, root.path());
        assert_eq!(
            get_environment("original", &env, root.path())
                .unwrap()
                .dev_ui_port,
            Some(port)
        );
        let metas = list_environments(&env, root.path()).unwrap();
        assert_ne!(
            crate::store::gateway_ports::choose_available_gateway_port(port, &metas, &env),
            port
        );
        let mut legacy = original.clone();
        legacy.name = "legacy".to_string();
        legacy.gateway_port = None;
        legacy.dev_ui_port = None;
        // An older environment without a selected Gateway must also avoid
        // the retained UI address when its effective port is computed.
        let mut legacy_envs = [original.clone(), legacy];
        let baseline = crate::store::gateway_ports::resolve_effective_gateway_ports(
            &legacy_envs,
            &env,
        )["legacy"];
        legacy_envs[0].dev_ui_port = Some(baseline);
        let effective =
            crate::store::gateway_ports::resolve_effective_gateway_ports(&legacy_envs, &env);
        let legacy_port = effective["legacy"];
        let (start, end) = crate::store::gateway_ports::openclaw_port_family_range(legacy_port);
        assert!(!(start..=end).contains(&baseline));
        crate::store::restore_env_snapshot(
            RestoreEnvSnapshotOptions {
                env_name: original.name.clone(),
                snapshot_id: snapshot.id,
            },
            &env,
            root.path(),
        )
        .unwrap();
        assert_eq!(
            get_environment("original", &env, root.path())
                .unwrap()
                .dev_ui_port,
            Some(port)
        );
        let cloned = crate::store::clone_environment(
            CloneEnvironmentOptions {
                source_name: original.name.clone(),
                name: "cloned".to_string(),
                root: None,
            },
            &env,
            root.path(),
        )
        .unwrap();
        assert_eq!(cloned.dev_ui_port, None);
        let clone_port = reserve_for("cloned", &env, root.path())
            .dev_ui_port
            .unwrap();
        assert_ne!(clone_port, port);
        erase_unaware_writer_fields(&env, root.path());
        assert_eq!(
            get_environment("cloned", &env, root.path())
                .unwrap()
                .dev_ui_port,
            Some(clone_port)
        );
        let archive = crate::store::export_environment(
            ExportEnvironmentOptions {
                name: original.name.clone(),
                output: Some(root.path().join("original.tar").display().to_string()),
            },
            &env,
            root.path(),
        )
        .unwrap();
        crate::store::import_environment(
            ImportEnvironmentOptions {
                archive: archive.archive_path,
                name: Some("imported".to_string()),
                root: None,
            },
            &env,
            root.path(),
        )
        .unwrap();
        assert_eq!(
            get_environment("imported", &env, root.path())
                .unwrap()
                .dev_ui_port,
            None
        );
        let imported_port = reserve_for("imported", &env, root.path())
            .dev_ui_port
            .unwrap();
        assert_ne!(imported_port, port);
        assert_ne!(imported_port, clone_port);
        let logs = crate::store::derive_env_paths(Path::new(&original.root))
            .state_dir
            .join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("gateway.log"), "old runtime output\n").unwrap();
        assert!(crate::store::repair_openclaw_runtime_state(&original, &env).unwrap());
        assert!(!logs.exists());
        assert_eq!(
            get_environment("original", &env, root.path())
                .unwrap()
                .dev_ui_port,
            Some(port)
        );
        remove_environment("original", false, &env, root.path()).unwrap();
        let saved = load(&env, root.path()).unwrap();
        assert!(!saved.ports.contains_key("original"));
        assert!(saved.ports.contains_key("cloned"));
        assert_eq!(create("original", &env, root.path()).dev_ui_port, None);
    }

    #[test]
    fn dev_ui_ports_refuse_stale_identity_and_invalid_metadata() {
        let root = tempfile::tempdir().unwrap();
        let env = environment(root.path());
        let mut meta = create("identity", &env, root.path());
        meta.dev_ui_port = Some(29999);
        save_environment(meta.clone(), &env, root.path()).unwrap();
        assert_eq!(
            get_environment("identity", &env, root.path())
                .unwrap()
                .dev_ui_port,
            None
        );
        let reserved = reserve_for(&meta.name, &env, root.path());
        let mut stale = reserved.clone();
        stale.created_at += time::Duration::seconds(1);
        let mut moved = reserved.clone();
        moved.root = root.path().join("replacement").display().to_string();
        for replaced in [&stale, &moved] {
            save_environment(replaced.clone(), &env, root.path()).unwrap();
            assert_eq!(
                get_environment("identity", &env, root.path())
                    .unwrap()
                    .dev_ui_port,
                None
            );
            remove(replaced, &env, root.path()).unwrap();
            assert!(
                load(&env, root.path())
                    .unwrap()
                    .ports
                    .contains_key("identity")
            );
        }
        save_environment(reserved.clone(), &env, root.path()).unwrap();
        let saved_path = path(&env, root.path()).unwrap();
        let bytes = fs::read(&saved_path).unwrap();
        let mut oversized = load(&env, root.path()).unwrap();
        oversized
            .ports
            .get_mut("identity")
            .unwrap()
            .env_root
            .push_str(&"x".repeat(MAX_BYTES as usize));
        assert!(write(&oversized, &env, root.path()).is_err());
        assert_eq!(fs::read(&saved_path).unwrap(), bytes);
        let mut invalid: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        invalid["ports"]["identity"]["port"] = 0.into();
        fs::write(&saved_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(get_environment("identity", &env, root.path()).is_err());
        assert!(reserve_for_result(&meta.name, &env, root.path()).is_err());
        let registry = crate::store::env_registry_path(&env, root.path()).unwrap();
        let registry_bytes = fs::read(&registry).unwrap();
        fs::remove_file(&registry).unwrap();
        let unpublished = root.path().join("unpublished");
        assert!(
            create_environment(
                CreateEnvironmentOptions {
                    name: "fresh".to_string(),
                    root: Some(unpublished.display().to_string()),
                    gateway_port: Some(21999),
                    service_enabled: false,
                    service_running: false,
                    default_runtime: None,
                    default_launcher: None,
                    dev: None,
                    protected: false,
                },
                &env,
                root.path(),
            )
            .is_err()
        );
        assert!(!unpublished.exists());
        assert!(!registry.exists());
        fs::write(&registry, registry_bytes).unwrap();
        fs::write(&saved_path, bytes).unwrap();
        assert_eq!(
            get_environment("identity", &env, root.path())
                .unwrap()
                .dev_ui_port,
            reserved.dev_ui_port
        );
    }

    fn reserve_for_result(
        name: &str,
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<EnvMeta, String> {
        let _operation = crate::store::lock_environment_operation(name, env, cwd)?;
        crate::store::with_locked_environments(env, cwd, |metas| {
            let mut meta = metas.iter().find(|meta| meta.name == name).unwrap().clone();
            meta.dev_ui_port = Some(reserve(&meta, metas, &[], env, cwd)?);
            Ok(meta)
        })
    }
}
