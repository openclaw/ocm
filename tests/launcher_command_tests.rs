mod support;

use std::fs;
use std::sync::Barrier;
use std::thread;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};
use serde_json::Value;

#[test]
fn launcher_list_uses_launcher_wording_when_empty() {
    let root = TestDir::new("launcher-list-empty");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let list = run_ocm(&cwd, &env, &["launcher", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    assert_eq!(stdout(&list), "No launchers.\n");
}

#[test]
fn launcher_add_and_list_use_launcher_storage() {
    let root = TestDir::new("launcher-add-list");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "stable",
            "--command",
            "sh",
            "--description",
            "launcher alias",
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    assert!(stdout(&add).contains("Added launcher stable"));

    let list = run_ocm(&cwd, &env, &["launcher", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    assert!(stdout(&list).contains("stable  sh"));

    let show_json = run_ocm(&cwd, &env, &["launcher", "show", "stable", "--json"]);
    assert!(show_json.status.success(), "{}", stderr(&show_json));
    let output = stdout(&show_json);
    assert!(output.contains("\"name\": \"stable\""));
    assert!(output.contains("\"description\": \"launcher alias\""));
}

#[test]
fn concurrent_launcher_adds_of_one_name_create_it_once() {
    const CALLERS: usize = 8;
    const ROUNDS: usize = 3;

    for round in 0..ROUNDS {
        let root = TestDir::new(&format!("launcher-add-concurrent-{round}"));
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        let start = Barrier::new(CALLERS);

        let outputs = thread::scope(|scope| {
            let callers = (0..CALLERS)
                .map(|caller| {
                    let (cwd, env, start) = (&cwd, &env, &start);
                    scope.spawn(move || {
                        let command = format!("cmd{caller}");
                        start.wait();
                        let output = run_ocm(
                            cwd,
                            env,
                            &["launcher", "add", "race", "--command", &command],
                        );
                        (command, output)
                    })
                })
                .collect::<Vec<_>>();
            callers
                .into_iter()
                .map(|caller| caller.join().unwrap())
                .collect::<Vec<_>>()
        });

        let added = outputs
            .iter()
            .filter(|(_, output)| output.status.success())
            .map(|(command, _)| command.as_str())
            .collect::<Vec<_>>();
        assert_eq!(added.len(), 1, "round {round}: added by {added:?}");
        for (_, output) in outputs
            .iter()
            .filter(|(_, output)| !output.status.success())
        {
            assert!(
                stderr(output).contains("launcher \"race\" already exists"),
                "round {round}: {}",
                stderr(output)
            );
        }

        let show = run_ocm(&cwd, &env, &["launcher", "show", "race", "--json"]);
        assert!(show.status.success(), "{}", stderr(&show));
        let stored: Value = serde_json::from_str(&stdout(&show)).unwrap();
        assert_eq!(stored["command"], added[0], "round {round}");
    }
}

#[test]
fn launcher_show_and_remove_use_launcher_metadata() {
    let root = TestDir::new("launcher-show-remove");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "stable",
            "--command",
            "sh",
            "--cwd",
            "./launcher-dir",
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let show = run_ocm(&cwd, &env, &["launcher", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("kind: ocm-launcher"));
    assert!(show_stdout.contains("name: stable"));
    assert!(show_stdout.contains("command: sh"));

    let remove = run_ocm(&cwd, &env, &["launcher", "remove", "stable"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(stdout(&remove).contains("Removed launcher stable"));

    let launcher_list = run_ocm(&cwd, &env, &["launcher", "list"]);
    assert!(launcher_list.status.success(), "{}", stderr(&launcher_list));
    assert_eq!(stdout(&launcher_list), "No launchers.\n");
}

#[test]
fn launcher_remove_rejects_bound_environments_without_changing_state() {
    let root = TestDir::new("launcher-remove-bound");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "sh"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let remove = run_ocm(&cwd, &env, &["launcher", "remove", "stable"]);
    assert!(!remove.status.success());
    assert!(stderr(&remove).contains(
        "launcher \"stable\" is still used by environment(s): demo; clear those bindings before removing it"
    ));

    let launcher = run_ocm(&cwd, &env, &["launcher", "show", "stable", "--json"]);
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let launcher: Value = serde_json::from_str(&stdout(&launcher)).unwrap();
    assert_eq!(launcher["name"], "stable");

    let environment = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(environment.status.success(), "{}", stderr(&environment));
    let environment: Value = serde_json::from_str(&stdout(&environment)).unwrap();
    assert_eq!(environment["defaultLauncher"], "stable");
}

#[test]
fn version_command_group_is_removed() {
    let root = TestDir::new("version-command-removed");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["version", "list"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("unknown command group: version"));
}
