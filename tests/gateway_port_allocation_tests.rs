mod support;

use std::fs;

use serde_json::{Value, json};

use crate::support::{TestDir, ocm_env, run_ocm, stderr, write_text};

#[test]
fn exhausted_clone_and_import_preserve_source_registry_and_destination() {
    let root = TestDir::new("gateway-port-boundary");
    let env = ocm_env(&root);
    let create = run_ocm(
        root.path(),
        &env,
        &["env", "create", "edge", "--port", "65425"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let config = root.child("ocm-home/envs/edge/.openclaw/openclaw.json");
    write_text(&config, "{\"gateway\":{\"port\":65425}}");
    let export = run_ocm(
        root.path(),
        &env,
        &["env", "export", "edge", "--output", "edge.tar"],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let registry = root.child("ocm-home/envs.json");
    let before = fs::read(&registry).unwrap();
    let config_before = fs::read(&config).unwrap();
    for existing_empty_root in [false, true] {
        let target = root.child("destination");
        if existing_empty_root {
            fs::create_dir(&target).unwrap();
        }
        for args in [
            vec![
                "env",
                "clone",
                "edge",
                "copy",
                "--root",
                target.to_str().unwrap(),
            ],
            vec![
                "env",
                "import",
                "edge.tar",
                "--name",
                "copy",
                "--root",
                target.to_str().unwrap(),
            ],
        ] {
            let result = run_ocm(root.path(), &env, &args);
            assert!(!result.status.success());
            assert!(
                stderr(&result).contains("no available Gateway port"),
                "{}",
                stderr(&result)
            );
            assert_eq!(fs::read(&registry).unwrap(), before);
            assert_eq!(fs::read(&config).unwrap(), config_before);
            assert_eq!(target.exists(), existing_empty_root);
            if existing_empty_root {
                assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
            }
        }
    }
}

#[test]
fn exhausted_create_and_legacy_resolution_return_errors_without_writes() {
    let root = TestDir::new("gateway-port-exhaustion");
    let env = ocm_env(&root);
    let create = run_ocm(
        root.path(),
        &env,
        &["env", "create", "seed", "--port", "18789"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let registry_path = root.child("ocm-home/envs.json");
    let mut registry: Value = serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    let template = registry["envs"][0].clone();
    let entries = registry["envs"].as_array_mut().unwrap();
    entries.clear();
    // Reserve the full automatic search domain without opening real sockets.
    for port in (18_789..=65_535).step_by(111) {
        let mut meta = template.clone();
        meta["name"] = json!(format!("reserved-{port}"));
        meta["root"] = json!(root.child(format!("reserved-{port}")).to_str().unwrap());
        meta["gatewayPort"] = json!(port);
        entries.push(meta);
    }
    fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let before = fs::read(&registry_path).unwrap();
    let create = run_ocm(root.path(), &env, &["env", "create", "full"]);
    assert!(!create.status.success());
    assert!(
        stderr(&create).contains("no available Gateway port"),
        "{}",
        stderr(&create)
    );
    assert!(!root.child("ocm-home/envs/full").exists());
    assert_eq!(fs::read(&registry_path).unwrap(), before);

    let mut legacy = template;
    legacy["name"] = json!("legacy");
    legacy["gatewayPort"] = Value::Null;
    registry["envs"].as_array_mut().unwrap().push(legacy);
    fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let before = fs::read(&registry_path).unwrap();
    for args in [
        vec!["env", "list", "--json"],
        vec!["env", "show", "legacy", "--json"],
    ] {
        let result = run_ocm(root.path(), &env, &args);
        assert!(!result.status.success());
        assert!(
            stderr(&result).contains("no available Gateway port"),
            "{}",
            stderr(&result)
        );
        assert_eq!(fs::read(&registry_path).unwrap(), before);
    }
    // Disabled legacy environments do not need an endpoint for service planning.
    // Their exhaustion must not block status for a sibling with an explicit port.
    let status = run_ocm(
        root.path(),
        &env,
        &["service", "status", "reserved-18789", "--json"],
    );
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(fs::read(&registry_path).unwrap(), before);

    // Leave one discovered free family available. The first legacy environment
    // keeps that valid assignment even when a later legacy environment fails.
    let entries = registry["envs"].as_array_mut().unwrap();
    let (available_port, listeners) = entries
        .iter()
        .filter_map(|meta| meta["gatewayPort"].as_u64())
        .filter(|port| *port < 40_000)
        .find_map(|port| {
            let listeners = (port..=port + 110)
                .map(|port| std::net::TcpListener::bind(("127.0.0.1", port as u16)))
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            Some((port, listeners))
        })
        .expect("a free test port family");
    entries.retain(|meta| meta["gatewayPort"] != available_port);
    let mut later = entries.last().unwrap().clone();
    later["name"] = json!("z-legacy");
    later["root"] = json!(root.child("z-legacy").to_str().unwrap());
    entries.push(later);
    fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let before = fs::read(&registry_path).unwrap();
    drop(listeners);
    let earlier = run_ocm(root.path(), &env, &["env", "show", "legacy", "--json"]);
    assert!(earlier.status.success(), "{}", stderr(&earlier));
    let shown: Value = serde_json::from_slice(&earlier.stdout).unwrap();
    assert_eq!(shown["gatewayPort"], available_port);
    let later = run_ocm(root.path(), &env, &["env", "show", "z-legacy", "--json"]);
    assert!(!later.status.success());
    assert!(
        stderr(&later).contains("no available Gateway port"),
        "{}",
        stderr(&later)
    );
    assert_eq!(fs::read(&registry_path).unwrap(), before);
}
