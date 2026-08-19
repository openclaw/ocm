mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use ocm::infra::download::file_sha256;
use ocm::store::{runtime_install_root, runtime_meta_path};
use serde_json::Value;
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, install_fake_managed_node_archive, install_fake_node_and_npm, ocm_env,
    openclaw_package_tarball as openclaw_package_tarball_with_version, path_string, run_ocm,
    stderr, stdout, write_executable_script,
};

fn append_tar_file(
    builder: &mut Builder<&mut GzEncoder<Vec<u8>>>,
    path: &str,
    body: &[u8],
    mode: u32,
) {
    let mut header = Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    builder.append_data(&mut header, path, body).unwrap();
}

fn openclaw_package_tarball(script_body: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        append_tar_file(
            &mut builder,
            "package/openclaw.mjs",
            script_body.as_bytes(),
            0o755,
        );
        append_tar_file(
            &mut builder,
            "package/package.json",
            br#"{"name":"openclaw","version":"2026.3.24","bin":{"openclaw":"openclaw.mjs"}}"#,
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn openclaw_package_tarball_with_dependencies(script_body: &str, version: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        append_tar_file(
            &mut builder,
            "package/openclaw.mjs",
            script_body.as_bytes(),
            0o755,
        );
        append_tar_file(
            &mut builder,
            "package/package.json",
            format!(
                "{{\"name\":\"openclaw\",\"version\":\"{version}\",\"bin\":{{\"openclaw\":\"openclaw.mjs\"}},\"dependencies\":{{\"chokidar\":\"^5.0.0\"}}}}"
            )
            .as_bytes(),
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn openclaw_package_tarball_with_bundled_extension(script_body: &str, version: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        append_tar_file(
            &mut builder,
            "package/openclaw.mjs",
            script_body.as_bytes(),
            0o755,
        );
        append_tar_file(
            &mut builder,
            "package/package.json",
            format!(
                "{{\"name\":\"openclaw\",\"version\":\"{version}\",\"bin\":{{\"openclaw\":\"openclaw.mjs\"}}}}"
            )
            .as_bytes(),
            0o644,
        );
        append_tar_file(
            &mut builder,
            "package/dist/extensions/memory-core/package.json",
            br#"{"name":"@openclaw/memory-core","version":"2026.4.25"}"#,
            0o644,
        );
        append_tar_file(
            &mut builder,
            "package/dist/extensions/memory-core/openclaw.plugin.json",
            br#"{"id":"memory-core","configSchema":{"type":"object"}}"#,
            0o644,
        );
        append_tar_file(
            &mut builder,
            "package/dist/extensions/memory-core/index.js",
            b"export const build = 'packaged';\n",
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn source_extension_package_tarball() -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        append_tar_file(
            &mut builder,
            "package/package.json",
            br#"{"name":"@openclaw/codex","version":"2026.4.25","dependencies":{"codex-runtime":"1.0.0"}}"#,
            0o644,
        );
        append_tar_file(
            &mut builder,
            "package/openclaw.plugin.json",
            br#"{"id":"codex","configSchema":{"type":"object"}}"#,
            0o644,
        );
        append_tar_file(
            &mut builder,
            "package/index.js",
            b"export const build = 'source';\n",
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn sha512_integrity(body: &[u8]) -> String {
    let digest = Sha512::digest(body);
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

fn installed_openclaw_runtime_entrypoint(install_root: &std::path::Path) -> std::path::PathBuf {
    install_root.join("files/node_modules/openclaw/openclaw.mjs")
}

fn install_fake_node_and_packing_npm(
    root: &TestDir,
    env: &mut BTreeMap<String, String>,
    archive_path: &Path,
) -> PathBuf {
    let bin_dir = root.child("fake-pack-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let log_path = root.child("fake-pack-npm.log");
    write_executable_script(
        &bin_dir.join("node"),
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf 'v22.22.3\\n'\n  exit 0\nfi\ncase \"$(basename \"$1\")\" in\n  ocm-npm-workspace-deps.mjs|ocm-npm-workspace-deps.mts)\n    script=\"$1\"\n    shift\n    exec \"$script\" \"$@\"\n    ;;\nesac\nprintf 'fake node\\n'\n",
    );
    let npm_script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> "{}"
if [ "$1" = "--version" ]; then
  printf '10.0.0\n'
  exit 0
fi

if [ "$1" = "pack" ]; then
  case " $* " in
    *" --json "*)
      shift
      destination=""
      source_dir=""
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --pack-destination)
            shift
            destination="$1"
            ;;
          --json|--ignore-scripts)
            ;;
          *)
            source_dir="$1"
            ;;
        esac
        shift
      done
      if [ -z "$destination" ] || [ -z "$source_dir" ] || [ -z "$FAKE_SOURCE_EXTENSION_ARCHIVE" ]; then
        echo "fake npm source extension pack is not configured" >&2
        exit 1
      fi
      mkdir -p "$destination"
      cp "$FAKE_SOURCE_EXTENSION_ARCHIVE" "$destination/$FAKE_SOURCE_EXTENSION_FILENAME"
      printf '[{{"name":"%s","version":"2026.4.25","filename":"%s"}}]\n' \
        "$FAKE_SOURCE_EXTENSION_NAME" "$FAKE_SOURCE_EXTENSION_FILENAME"
      exit 0
      ;;
  esac
  printf 'verify-deps-before-run=%s\n' "$PNPM_CONFIG_VERIFY_DEPS_BEFORE_RUN" >> "{}"
  printf 'allow-unreleased-changelog=%s\n' "$OPENCLAW_PREPACK_ALLOW_UNRELEASED_CHANGELOG" >> "{}"
  printf 'ignore-scripts-lower=%s\n' "${{npm_config_ignore_scripts-unset}}" >> "{}"
  printf 'ignore-scripts-upper=%s\n' "${{NPM_CONFIG_IGNORE_SCRIPTS-unset}}" >> "{}"
  shift
  destination=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --pack-destination)
        shift
        destination="$1"
        ;;
    esac
    shift
  done
  if [ -z "$destination" ]; then
    echo "fake npm pack expected --pack-destination" >&2
    exit 1
  fi
  mkdir -p "$destination"
  cp "{}" "$destination/openclaw-2026.4.25.tgz"
  printf 'openclaw-2026.4.25.tgz\n'
  exit 0
fi

prefix=""
archive=""
source_extension_archive=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      shift
      prefix="$1"
      ;;
    install|--omit=dev|--no-save|--package-lock=false)
      ;;
    *)
      source_extension_archive="$archive"
      archive="$1"
      ;;
  esac
  shift
done

if [ -z "$prefix" ] || [ -z "$archive" ]; then
  echo "fake npm expected --prefix and archive path" >&2
  exit 1
fi

mkdir -p "$prefix/node_modules/openclaw"
tar -xzf "$archive" -C "$prefix/node_modules/openclaw" --strip-components=1 package
if [ -n "$source_extension_archive" ]; then
  source_extension_root="$prefix/node_modules/$FAKE_SOURCE_EXTENSION_INSTALL_PATH"
  mkdir -p "$source_extension_root"
  tar -xzf "$source_extension_archive" -C "$source_extension_root" --strip-components=1 package
  mkdir -p "$prefix/node_modules/codex-runtime"
  printf '{{"name":"codex-runtime","version":"1.0.0"}}\n' > "$prefix/node_modules/codex-runtime/package.json"
fi
if grep -q '"chokidar"' "$prefix/node_modules/openclaw/package.json"; then
  mkdir -p "$prefix/node_modules/chokidar"
  mkdir -p "$prefix/node_modules/readdirp"
  mkdir -p "$prefix/node_modules/@scope/tool"
  printf '{{"name":"chokidar","version":"5.0.0"}}\n' > "$prefix/node_modules/chokidar/package.json"
  printf '{{"name":"readdirp","version":"4.1.2"}}\n' > "$prefix/node_modules/readdirp/package.json"
  printf '{{"name":"@scope/tool","version":"1.0.0"}}\n' > "$prefix/node_modules/@scope/tool/package.json"
fi
"#,
        path_string(&log_path),
        path_string(&log_path),
        path_string(&log_path),
        path_string(&log_path),
        path_string(&log_path),
        path_string(archive_path),
    );
    write_executable_script(&bin_dir.join("npm"), &npm_script);

    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(&bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(&bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
    log_path
}

#[test]
fn runtime_list_uses_runtime_wording_when_empty() {
    let root = TestDir::new("runtime-list-empty");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let list = run_ocm(&cwd, &env, &["runtime", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    assert_eq!(stdout(&list), "No runtimes.\n");
}

#[test]
fn runtime_add_and_list_use_runtime_storage() {
    let root = TestDir::new("runtime-add-list");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("stable"), "#!/bin/sh\n").unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "stable",
            "--path",
            "./bin/stable",
            "--description",
            "stable runtime",
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    assert!(stdout(&add).contains("Added runtime stable"));

    let list = run_ocm(&cwd, &env, &["runtime", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    assert!(stdout(&list).contains("stable"));
    assert!(stdout(&list).contains("/bin/stable"));

    let show_json = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(show_json.status.success(), "{}", stderr(&show_json));
    let output = stdout(&show_json);
    assert!(output.contains("\"name\": \"stable\""));
    assert!(output.contains("\"description\": \"stable runtime\""));
}

#[test]
fn runtime_show_and_remove_use_runtime_metadata() {
    let root = TestDir::new("runtime-show-remove");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("stable"), "#!/bin/sh\n").unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("kind: ocm-runtime"));
    assert!(show_stdout.contains("name: stable"));
    assert!(show_stdout.contains("binaryPath:"));
    assert!(show_stdout.contains("sourceKind: registered"));

    let remove = run_ocm(&cwd, &env, &["runtime", "remove", "stable"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(stdout(&remove).contains("Removed runtime stable"));

    let runtime_list = run_ocm(&cwd, &env, &["runtime", "list"]);
    assert!(runtime_list.status.success(), "{}", stderr(&runtime_list));
    assert_eq!(stdout(&runtime_list), "No runtimes.\n");
}

#[test]
fn runtime_remove_rejects_a_runtime_bound_to_an_environment() {
    let root = TestDir::new("runtime-remove-bound");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let remove = run_ocm(&cwd, &env, &["runtime", "remove", "stable"]);
    assert_eq!(remove.status.code(), Some(1));
    let error = stderr(&remove);
    assert!(
        error.contains("runtime \"stable\" is still used"),
        "{error}"
    );
    assert!(error.contains("\"demo\""), "{error}");
    assert!(error.contains("ocm upgrade"), "{error}");

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let environment = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(environment.status.success(), "{}", stderr(&environment));
    assert!(stdout(&environment).contains("\"defaultRuntime\": \"stable\""));
}

#[test]
fn runtime_add_rejects_a_name_referenced_by_a_stale_binding() {
    let root = TestDir::new("runtime-add-stale-binding");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    fs::remove_file(root.child("ocm-home/runtimes/stable.json")).unwrap();

    let replacement = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert_eq!(replacement.status.code(), Some(1));
    let error = stderr(&replacement);
    assert!(
        error.contains("runtime \"stable\" is still used"),
        "{error}"
    );
    assert!(error.contains("\"demo\""), "{error}");
    assert!(!root.child("ocm-home/runtimes/stable.json").exists());
}

#[test]
fn runtime_install_and_which_use_the_managed_binary_path() {
    let root = TestDir::new("runtime-install-which");
    let cwd = root.child("workspace");
    let source_dir = cwd.join("downloads");
    let source_path = source_dir.join("openclaw");
    fs::create_dir_all(&source_dir).unwrap();
    write_executable_script(&source_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./downloads/openclaw",
            "--description",
            "managed runtime",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(stdout(&install).contains("Installed runtime stable"));
    assert!(stdout(&install).contains("use in env: ocm env create mira --runtime stable"));

    let install_root = runtime_install_root("stable", &env, &cwd).unwrap();
    let expected_binary = install_root.join("files/openclaw");
    let expected_source_path = fs::canonicalize(&source_path).unwrap();
    let which = run_ocm(&cwd, &env, &["runtime", "which", "stable"]);
    assert!(which.status.success(), "{}", stderr(&which));
    assert_eq!(
        stdout(&which),
        format!("{}\n", path_string(&expected_binary))
    );

    let which_json = run_ocm(&cwd, &env, &["runtime", "which", "stable", "--json"]);
    assert!(which_json.status.success(), "{}", stderr(&which_json));
    let which_json_stdout = stdout(&which_json);
    assert!(which_json_stdout.contains("\"name\": \"stable\""));
    assert!(which_json_stdout.contains(&format!(
        "\"binaryPath\": \"{}\"",
        path_string(&expected_binary)
    )));
    assert!(which_json_stdout.contains("\"sourceKind\": \"installed\""));
    assert!(which_json_stdout.contains(&format!(
        "\"installRoot\": \"{}\"",
        path_string(&install_root)
    )));

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("sourceKind: installed"));
    assert!(show_stdout.contains(&format!(
        "sourcePath: {}",
        path_string(&expected_source_path)
    )));
    assert!(show_stdout.contains(&format!("installRoot: {}", path_string(&install_root))));
}

#[test]
fn runtime_install_from_url_downloads_into_the_managed_store() {
    let root = TestDir::new("runtime-install-url");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let server = TestHttpServer::serve_bytes(
        "/releases/openclaw-nightly",
        "application/octet-stream",
        b"downloaded-runtime",
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "nightly",
            "--url",
            &server.url(),
            "--description",
            "downloaded runtime",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(stdout(&install).contains("Installed runtime nightly"));

    let install_root = runtime_install_root("nightly", &env, &cwd).unwrap();
    let expected_binary = install_root.join("files/openclaw-nightly");
    assert_eq!(fs::read(&expected_binary).unwrap(), b"downloaded-runtime");

    let show = run_ocm(&cwd, &env, &["runtime", "show", "nightly"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("sourceKind: installed"));
    assert!(show_stdout.contains(&format!("sourceUrl: {}", server.url())));
    assert!(show_stdout.contains("description: downloaded runtime"));
}

#[test]
fn runtime_build_local_packs_and_installs_release_shaped_package() {
    let root = TestDir::new("runtime-build-local");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"},"scripts":{"prepack":"node --import tsx scripts/openclaw-prepack.ts"}}"#,
    )
    .unwrap();
    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball_with_version(
            "#!/usr/bin/env node\nconsole.log('local package runtime');\n",
            "2026.4.25",
        ),
    )
    .unwrap();

    let mut env = ocm_env(&root);
    env.insert("npm_config_ignore_scripts".to_string(), "true".to_string());
    env.insert("NPM_CONFIG_IGNORE_SCRIPTS".to_string(), "true".to_string());
    let npm_log = install_fake_node_and_packing_npm(&root, &mut env, &archive_path);

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-local",
            "--repo",
            "./openclaw",
            "--raw",
        ],
    );
    assert!(build.status.success(), "{}", stderr(&build));
    assert!(stdout(&build).contains("Built runtime main-local"));

    let npm_log = fs::read_to_string(npm_log).unwrap();
    assert!(npm_log.contains("pack --pack-destination"));
    assert!(npm_log.contains("verify-deps-before-run=false"));
    assert!(npm_log.contains("allow-unreleased-changelog=1"));
    assert!(npm_log.contains("ignore-scripts-lower=false"));
    assert!(npm_log.contains("ignore-scripts-upper=unset"));
    assert!(npm_log.contains("install --prefix"));

    let install_root = runtime_install_root("main-local", &env, &cwd).unwrap();
    let expected_binary = installed_openclaw_runtime_entrypoint(&install_root);
    assert!(expected_binary.exists());
    assert_eq!(
        fs::read_to_string(&expected_binary).unwrap(),
        "#!/usr/bin/env node\nconsole.log('local package runtime');\n"
    );

    let show_json = run_ocm(&cwd, &env, &["runtime", "show", "main-local", "--json"]);
    assert!(show_json.status.success(), "{}", stderr(&show_json));
    let value: Value = serde_json::from_str(&stdout(&show_json)).unwrap();
    assert_eq!(value["name"], "main-local");
    assert_eq!(value["sourceKind"], "installed");
    assert_eq!(value["releaseVersion"], "2026.4.25");
    assert_eq!(value["binaryPath"], path_string(&expected_binary));
    assert_eq!(
        value["sourcePath"],
        path_string(&fs::canonicalize(&repo).unwrap())
    );
    assert!(value["sourceSha256"].as_str().is_some());
    assert!(value["runtimeSha256"].as_str().is_some());

    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "main-local", "--raw"]);
    assert!(verify.status.success(), "{}", stderr(&verify));
}

#[cfg(unix)]
#[test]
fn runtime_build_local_rejects_dependencies_from_another_checkout_before_pack() {
    let root = TestDir::new("runtime-build-local-external-dependencies");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    let shared_dependencies = root.child("other-checkout/node_modules");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&shared_dependencies).unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"}}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink(&shared_dependencies, repo.join("node_modules")).unwrap();

    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball_with_version(
            "#!/usr/bin/env node\nconsole.log('wrong dependency tree');\n",
            "2026.4.25",
        ),
    )
    .unwrap();
    let mut env = ocm_env(&root);
    let npm_log = install_fake_node_and_packing_npm(&root, &mut env, &archive_path);

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-local",
            "--repo",
            &path_string(&repo),
            "--raw",
        ],
    );

    assert!(!build.status.success());
    let error = stderr(&build);
    assert!(
        error.contains("dependencies resolve outside the selected checkout"),
        "{error}"
    );
    assert!(
        error.contains(&path_string(&shared_dependencies)),
        "{error}"
    );
    assert!(error.contains("standalone checkout"), "{error}");
    assert!(
        !npm_log.exists(),
        "npm pack ran before dependency validation"
    );
}

#[cfg(unix)]
#[test]
fn runtime_build_local_rejects_dangling_dependency_link_before_pack() {
    let root = TestDir::new("runtime-build-local-dangling-dependencies");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    let missing_dependencies = root.child("missing-checkout/node_modules");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"}}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink(&missing_dependencies, repo.join("node_modules")).unwrap();

    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball_with_version(
            "#!/usr/bin/env node\nconsole.log('wrong dependency tree');\n",
            "2026.4.25",
        ),
    )
    .unwrap();
    let mut env = ocm_env(&root);
    let npm_log = install_fake_node_and_packing_npm(&root, &mut env, &archive_path);

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-local",
            "--repo",
            &path_string(&repo),
            "--raw",
        ],
    );

    assert!(!build.status.success());
    assert!(
        stderr(&build).contains("failed to resolve OpenClaw dependencies"),
        "{}",
        stderr(&build)
    );
    assert!(
        !npm_log.exists(),
        "npm pack ran before dependency validation"
    );
}

#[test]
fn runtime_verify_reports_package_tree_drift() {
    let root = TestDir::new("runtime-verify-package-tree-drift");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"}}"#,
    )
    .unwrap();
    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball_with_version(
            "#!/usr/bin/env node\nconsole.log('local package runtime');\n",
            "2026.4.25",
        ),
    )
    .unwrap();

    let mut env = ocm_env(&root);
    let _npm_log = install_fake_node_and_packing_npm(&root, &mut env, &archive_path);
    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-local",
            "--repo",
            "./openclaw",
        ],
    );
    assert!(build.status.success(), "{}", stderr(&build));

    let install_root = runtime_install_root("main-local", &env, &cwd).unwrap();
    fs::write(
        install_root.join("files/node_modules/openclaw/package.json"),
        br#"{"name":"openclaw","version":"tampered"}"#,
    )
    .unwrap();

    let tree_drift_which = run_ocm(&cwd, &env, &["runtime", "which", "main-local", "--raw"]);
    assert!(
        tree_drift_which.status.success(),
        "{}",
        stderr(&tree_drift_which)
    );

    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "main-local", "--json"]);
    assert_eq!(verify.status.code(), Some(1));
    let value: Value = serde_json::from_str(&stdout(&verify)).unwrap();
    assert_eq!(value["healthy"], false);
    assert!(
        value["issue"]
            .as_str()
            .unwrap()
            .contains("runtime sha256 mismatch:")
    );

    let meta_path = runtime_meta_path("main-local", &env, &cwd).unwrap();
    let mut legacy_meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    legacy_meta.as_object_mut().unwrap().remove("runtimeSha256");
    fs::write(&meta_path, serde_json::to_vec_pretty(&legacy_meta).unwrap()).unwrap();
    let legacy_verify = run_ocm(&cwd, &env, &["runtime", "verify", "main-local", "--json"]);
    assert!(legacy_verify.status.success(), "{}", stderr(&legacy_verify));

    fs::write(
        install_root.join("files/node_modules/openclaw/openclaw.mjs"),
        "tampered launcher\n",
    )
    .unwrap();
    let launcher_verify = run_ocm(&cwd, &env, &["runtime", "verify", "main-local", "--json"]);
    assert_eq!(launcher_verify.status.code(), Some(1));
    assert!(stdout(&launcher_verify).contains("sha256 mismatch:"));

    let launcher_which = run_ocm(&cwd, &env, &["runtime", "which", "main-local", "--raw"]);
    assert_eq!(launcher_which.status.code(), Some(1));
    assert!(stderr(&launcher_which).contains("sha256 mismatch:"));
}

#[test]
fn runtime_build_local_can_include_source_extensions_without_widening_default_trust() {
    let root = TestDir::new("runtime-build-local-source-extensions");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    let codex_dir = repo.join("dist/extensions/codex");
    let memory_core_dir = repo.join("dist/extensions/memory-core");
    let dependency_only_dir = repo.join("dist/extensions/node_modules/external-helper");
    let external_plugin_dir = cwd.join("external-plugins/external-demo");
    for dir in [
        &codex_dir,
        &memory_core_dir,
        &dependency_only_dir,
        &external_plugin_dir,
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"}}"#,
    )
    .unwrap();
    fs::write(
        codex_dir.join("package.json"),
        br#"{"name":"@openclaw/codex","version":"2026.4.25","dependencies":{"codex-runtime":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        codex_dir.join("openclaw.plugin.json"),
        br#"{"id":"codex","configSchema":{"type":"object"}}"#,
    )
    .unwrap();
    fs::write(
        codex_dir.join("index.js"),
        "export const build = 'source';\n",
    )
    .unwrap();
    fs::write(
        memory_core_dir.join("package.json"),
        br#"{"name":"@openclaw/memory-core","version":"2026.4.25"}"#,
    )
    .unwrap();
    fs::write(
        memory_core_dir.join("openclaw.plugin.json"),
        br#"{"id":"memory-core","configSchema":{"type":"object"}}"#,
    )
    .unwrap();
    fs::write(
        memory_core_dir.join("index.js"),
        "export const build = 'source';\n",
    )
    .unwrap();
    fs::write(
        dependency_only_dir.join("package.json"),
        br#"{"name":"external-helper","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        external_plugin_dir.join("package.json"),
        br#"{"name":"external-demo","version":"1.0.0"}"#,
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        &external_plugin_dir,
        repo.join("dist/extensions/linked-external-demo"),
    )
    .unwrap();

    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball_with_bundled_extension(
            "#!/usr/bin/env node\nconsole.log('source extension runtime');\n",
            "2026.4.25",
        ),
    )
    .unwrap();
    let source_extension_archive = root.child("openclaw-codex-2026.4.25.tgz");
    fs::write(
        &source_extension_archive,
        source_extension_package_tarball(),
    )
    .unwrap();

    let mut env = ocm_env(&root);
    env.insert(
        "FAKE_SOURCE_EXTENSION_ARCHIVE".to_string(),
        path_string(&source_extension_archive),
    );
    env.insert(
        "FAKE_SOURCE_EXTENSION_NAME".to_string(),
        "@openclaw/codex".to_string(),
    );
    env.insert(
        "FAKE_SOURCE_EXTENSION_FILENAME".to_string(),
        "openclaw-codex-2026.4.25.tgz".to_string(),
    );
    env.insert(
        "FAKE_SOURCE_EXTENSION_INSTALL_PATH".to_string(),
        "@openclaw/codex".to_string(),
    );
    install_fake_node_and_packing_npm(&root, &mut env, &archive_path);

    let release_build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "release-shaped",
            "--repo",
            "./openclaw",
            "--raw",
        ],
    );
    assert!(release_build.status.success(), "{}", stderr(&release_build));
    let release_root = runtime_install_root("release-shaped", &env, &cwd)
        .unwrap()
        .join("files/node_modules/openclaw");
    assert!(!release_root.join("dist/extensions/codex").exists());
    assert_eq!(
        fs::read_to_string(release_root.join("dist/extensions/memory-core/index.js")).unwrap(),
        "export const build = 'packaged';\n"
    );

    let source_build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "source-extensions",
            "--repo",
            "./openclaw",
            "--include-source-extensions",
            "--raw",
        ],
    );
    assert!(source_build.status.success(), "{}", stderr(&source_build));
    let source_root = runtime_install_root("source-extensions", &env, &cwd)
        .unwrap()
        .join("files/node_modules/openclaw");
    assert_eq!(
        fs::read_to_string(source_root.join("dist/extensions/codex/index.js")).unwrap(),
        "export const build = 'source';\n"
    );
    assert_eq!(
        fs::read_to_string(source_root.join("dist/extensions/memory-core/index.js")).unwrap(),
        "export const build = 'packaged';\n"
    );
    assert!(
        source_root
            .join("node_modules/codex-runtime/package.json")
            .exists()
    );
    assert!(!source_root.join("dist/extensions/external-helper").exists());
    assert!(!source_root.join("dist/extensions/external-demo").exists());
    assert!(
        !source_root
            .join("dist/extensions/linked-external-demo")
            .exists()
    );
}

#[test]
fn runtime_build_local_derives_source_plugins_from_target_env_before_pack() {
    let root = TestDir::new("runtime-build-local-target-source-plugins");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    let source_codex_dir = repo.join("extensions/codex");
    let built_codex_dir = repo.join("dist/extensions/codex");
    for dir in [&source_codex_dir, &built_codex_dir] {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("package.json"),
            br#"{"name":"@openclaw/codex","version":"2026.4.25","dependencies":{"codex-runtime":"1.0.0"}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("openclaw.plugin.json"),
            br#"{"id":"codex","configSchema":{"type":"object"}}"#,
        )
        .unwrap();
    }
    fs::write(
        built_codex_dir.join("index.js"),
        "export const build = 'source';\n",
    )
    .unwrap();
    let unrelated_dir = repo.join("dist/extensions/unrelated");
    fs::create_dir_all(&unrelated_dir).unwrap();
    fs::write(
        unrelated_dir.join("openclaw.plugin.json"),
        br#"{"id":"unrelated"}"#,
    )
    .unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"}}"#,
    )
    .unwrap();

    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball_with_bundled_extension(
            "#!/usr/bin/env node\nconsole.log('target runtime');\n",
            "2026.4.25",
        ),
    )
    .unwrap();
    let source_extension_archive = root.child("openclaw-codex-2026.4.25.tgz");
    fs::write(
        &source_extension_archive,
        source_extension_package_tarball(),
    )
    .unwrap();

    let mut env = ocm_env(&root);
    env.insert(
        "FAKE_SOURCE_EXTENSION_ARCHIVE".to_string(),
        path_string(&source_extension_archive),
    );
    env.insert(
        "FAKE_SOURCE_EXTENSION_NAME".to_string(),
        "@openclaw/codex".to_string(),
    );
    env.insert(
        "FAKE_SOURCE_EXTENSION_FILENAME".to_string(),
        "openclaw-codex-2026.4.25.tgz".to_string(),
    );
    env.insert(
        "FAKE_SOURCE_EXTENSION_INSTALL_PATH".to_string(),
        "@openclaw/codex".to_string(),
    );
    install_fake_node_and_packing_npm(&root, &mut env, &archive_path);

    let old_runtime = cwd.join("old-openclaw");
    write_executable_script(&old_runtime, "#!/bin/sh\nexit 0\n");
    let add = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "old-local",
            "--path",
            old_runtime.to_str().unwrap(),
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "primary", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    fs::write(
        root.child("ocm-home/envs/primary/.openclaw/openclaw.json"),
        br#"{"plugins":{"entries":{"codex":{"enabled":true}}}}"#,
    )
    .unwrap();

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "primary-test",
            "--repo",
            "./openclaw",
            "--for-env",
            "primary",
            "--raw",
        ],
    );
    assert!(build.status.success(), "{}", stderr(&build));
    let runtime_root = runtime_install_root("primary-test", &env, &cwd)
        .unwrap()
        .join("files/node_modules/openclaw");
    assert_eq!(
        fs::read_to_string(runtime_root.join("dist/extensions/codex/index.js")).unwrap(),
        "export const build = 'source';\n"
    );
}

#[test]
fn runtime_build_local_rejects_missing_target_plugin_before_pack() {
    let root = TestDir::new("runtime-build-local-missing-target-plugin");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    fs::create_dir_all(repo.join("extensions/codex")).unwrap();
    fs::write(
        repo.join("extensions/codex/openclaw.plugin.json"),
        br#"{"id":"codex"}"#,
    )
    .unwrap();
    fs::write(
        repo.join("extensions/codex/package.json"),
        br#"{"name":"@openclaw/codex","version":"2026.4.25"}"#,
    )
    .unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"}}"#,
    )
    .unwrap();

    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball("#!/usr/bin/env node\n"),
    )
    .unwrap();
    let mut env = ocm_env(&root);
    let npm_log = install_fake_node_and_packing_npm(&root, &mut env, &archive_path);
    let old_runtime = cwd.join("old-openclaw");
    write_executable_script(&old_runtime, "#!/bin/sh\nexit 0\n");
    let add = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "old-local",
            "--path",
            old_runtime.to_str().unwrap(),
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "primary", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    fs::write(
        root.child("ocm-home/envs/primary/.openclaw/openclaw.json"),
        br#"{"plugins":{"entries":{"missing-plugin":{"enabled":true}}}}"#,
    )
    .unwrap();

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "primary-test",
            "--repo",
            "./openclaw",
            "--for-env",
            "primary",
            "--raw",
        ],
    );
    assert!(!build.status.success(), "{}", stdout(&build));
    assert!(
        stderr(&build).contains("missing-plugin"),
        "{}",
        stderr(&build)
    );
    assert!(
        !npm_log.exists(),
        "npm pack must not run after preflight failure"
    );
}

#[test]
fn runtime_build_local_rejects_conflicting_source_plugin_modes() {
    let root = TestDir::new("runtime-build-local-conflicting-source-plugin-modes");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "primary-test",
            "--repo",
            "./openclaw",
            "--for-env",
            "primary",
            "--include-source-extensions",
        ],
    );

    assert!(!build.status.success(), "{}", stdout(&build));
    assert!(
        stderr(&build).contains(
            "runtime build-local accepts only one of --include-source-extensions or --for-env"
        ),
        "{}",
        stderr(&build)
    );
}

#[test]
fn runtime_build_local_rejects_a_bound_runtime_before_repo_validation() {
    let root = TestDir::new("runtime-build-local-bound");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("main-local"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "main-local", "--path", "./bin/main-local"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "main-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-local",
            "--repo",
            "./missing-openclaw",
            "--force",
        ],
    );
    assert_eq!(build.status.code(), Some(1));
    let error = stderr(&build);
    assert!(
        error.contains("runtime \"main-local\" is still used"),
        "{error}"
    );
    assert!(!error.contains("repo path does not exist"), "{error}");
}

fn assert_runtime_build_local_uses_repo_workspace_dependency_adapter(
    adapter_extension: &str,
    include_legacy_adapter: bool,
) {
    let root = TestDir::new(&format!(
        "runtime-build-local-workspace-adapter-{adapter_extension}"
    ));
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    let workspace_dependencies = [
        repo.join("packages/ai"),
        repo.join("packages/normalization-core"),
        repo.join("packages/session-url-contract"),
        repo.join("packages/gateway-protocol"),
    ];
    fs::create_dir_all(repo.join("scripts")).unwrap();
    for package_dir in &workspace_dependencies {
        fs::create_dir_all(package_dir).unwrap();
    }
    fs::write(
        repo.join("pnpm-workspace.yaml"),
        "packages:\n  - .\n  - packages/*\n",
    )
    .unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"},"dependencies":{"@openclaw/ai":"workspace:*"}}"#,
    )
    .unwrap();
    fs::write(
        workspace_dependencies[0].join("package.json"),
        br#"{"name":"@openclaw/ai","version":"2026.4.25","optionalDependencies":{"@openclaw/normalization-core":"workspace:*"}}"#,
    )
    .unwrap();
    fs::write(
        workspace_dependencies[1].join("package.json"),
        br#"{"name":"@openclaw/normalization-core","version":"0.0.0-private","peerDependencies":{"@openclaw/session-url-contract":"workspace:^"}}"#,
    )
    .unwrap();
    fs::write(
        workspace_dependencies[2].join("package.json"),
        br#"{"name":"@openclaw/session-url-contract","version":"0.0.0-private","devDependencies":{"@openclaw/gateway-protocol":"workspace:~"}}"#,
    )
    .unwrap();
    fs::write(
        workspace_dependencies[3].join("package.json"),
        br#"{"name":"@openclaw/gateway-protocol","version":"0.0.0-private","dependencies":{"@openclaw/ai":"workspace:*","openclaw":"workspace:*"}}"#,
    )
    .unwrap();

    let adapter_log = root.child("workspace-adapter.log");
    write_executable_script(
        &repo.join(format!(
            "scripts/ocm-npm-workspace-deps.{adapter_extension}"
        )),
        &format!(
            r#"#!/bin/sh
printf 'adapter={adapter_extension}\n' >> "{}"
printf 'args=%s\n' "$*" >> "{}"
printf 'real-npm=%s\n' "$OPENCLAW_OCM_REAL_NPM_BIN" >> "{}"
printf 'workspace-dirs=%s\n' "$OPENCLAW_OCM_WORKSPACE_DEPENDENCY_DIRS" >> "{}"
exec "$OPENCLAW_OCM_REAL_NPM_BIN" "$@"
"#,
            path_string(&adapter_log),
            path_string(&adapter_log),
            path_string(&adapter_log),
            path_string(&adapter_log),
        ),
    );
    if include_legacy_adapter {
        write_executable_script(
            &repo.join("scripts/ocm-npm-workspace-deps.mjs"),
            &format!(
                "#!/bin/sh\nprintf 'adapter=mjs\\n' >> \"{}\"\nexit 1\n",
                path_string(&adapter_log)
            ),
        );
    }

    let archive_path = root.child("packed-openclaw.tgz");
    fs::write(
        &archive_path,
        openclaw_package_tarball_with_version(
            "#!/usr/bin/env node\nconsole.log('local workspace package runtime');\n",
            "2026.4.25",
        ),
    )
    .unwrap();

    let mut env = ocm_env(&root);
    install_fake_node_and_packing_npm(&root, &mut env, &archive_path);
    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-workspace",
            "--repo",
            "./openclaw",
            "--raw",
        ],
    );

    assert!(build.status.success(), "{}", stderr(&build));
    let adapter_log = fs::read_to_string(adapter_log).unwrap();
    assert!(adapter_log.contains("args=pack --pack-destination"));
    assert!(adapter_log.contains("args=install --prefix"));
    assert!(adapter_log.contains("real-npm=npm"));
    assert!(adapter_log.contains(&format!("adapter={adapter_extension}")));
    if include_legacy_adapter {
        assert!(!adapter_log.contains("adapter=mjs"));
    }
    let expected_dirs = workspace_dependencies
        .into_iter()
        .map(|package_dir| fs::canonicalize(package_dir).unwrap())
        .collect::<BTreeSet<_>>();
    let workspace_dir_values = adapter_log
        .lines()
        .filter_map(|line| line.strip_prefix("workspace-dirs="))
        .collect::<Vec<_>>();
    assert_eq!(workspace_dir_values.len(), 2, "{adapter_log}");
    for value in workspace_dir_values {
        assert_eq!(
            std::env::split_paths(value).collect::<BTreeSet<_>>(),
            expected_dirs,
            "{adapter_log}"
        );
    }
}

#[test]
fn runtime_build_local_prefers_current_mts_workspace_dependency_adapter() {
    assert_runtime_build_local_uses_repo_workspace_dependency_adapter("mts", true);
}

#[test]
fn runtime_build_local_keeps_legacy_mjs_workspace_dependency_adapter() {
    assert_runtime_build_local_uses_repo_workspace_dependency_adapter("mjs", false);
}

#[test]
fn runtime_build_local_rejects_a_missing_transitive_workspace_dependency() {
    let root = TestDir::new("runtime-build-local-missing-transitive-workspace");
    let cwd = root.child("workspace");
    let repo = cwd.join("openclaw");
    let workspace_dependency = repo.join("packages/ai");
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::create_dir_all(&workspace_dependency).unwrap();
    fs::write(
        repo.join("pnpm-workspace.yaml"),
        "packages:\n  - .\n  - packages/*\n",
    )
    .unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"openclaw","version":"2026.4.25","bin":{"openclaw":"openclaw.mjs"},"dependencies":{"@openclaw/ai":"workspace:*"}}"#,
    )
    .unwrap();
    fs::write(
        workspace_dependency.join("package.json"),
        br#"{"name":"@openclaw/ai","version":"2026.4.25","dependencies":{"@openclaw/missing":"workspace:*"}}"#,
    )
    .unwrap();
    write_executable_script(
        &repo.join("scripts/ocm-npm-workspace-deps.mts"),
        "#!/bin/sh\nexit 0\n",
    );

    let env = ocm_env(&root);
    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-workspace",
            "--repo",
            "./openclaw",
            "--raw",
        ],
    );

    assert_eq!(build.status.code(), Some(1));
    assert!(
        stderr(&build).contains(
            "OpenClaw workspace dependency \"@openclaw/missing\" is not declared by pnpm-workspace.yaml"
        ),
        "{}",
        stderr(&build)
    );
}

#[test]
fn runtime_build_local_requires_an_openclaw_repo_package() {
    let root = TestDir::new("runtime-build-local-invalid-repo");
    let cwd = root.child("workspace");
    let repo = cwd.join("not-openclaw");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("package.json"),
        br#"{"name":"other","version":"1.0.0"}"#,
    )
    .unwrap();
    let env = ocm_env(&root);

    let build = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "build-local",
            "main-local",
            "--repo",
            "./not-openclaw",
        ],
    );
    assert!(!build.status.success());
    assert!(stderr(&build).contains("requires an OpenClaw repo package named \"openclaw\""));
}

#[test]
fn runtime_install_from_manifest_version_downloads_and_records_release_metadata() {
    let root = TestDir::new("runtime-install-manifest-version");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let artifact_body = b"release-binary";
    let digest_path = root.child("sha256/openclaw-0.2.0");
    fs::create_dir_all(digest_path.parent().unwrap()).unwrap();
    fs::write(&digest_path, artifact_body).unwrap();
    let sha256 = file_sha256(&digest_path).unwrap();

    let artifact_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.2.0",
        "application/octet-stream",
        artifact_body,
    );
    let manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\",\"description\":\"stable manifest runtime\"}}]}}",
        artifact_server.url(),
        sha256
    );
    let manifest_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body.as_bytes(),
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--version",
            "0.2.0",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(stdout(&install).contains("Installed runtime stable"));

    let install_root = runtime_install_root("stable", &env, &cwd).unwrap();
    let expected_binary = install_root.join("files/openclaw-0.2.0");
    assert_eq!(fs::read(&expected_binary).unwrap(), artifact_body);

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("sourceKind: installed"));
    assert!(show_stdout.contains(&format!("sourceManifestUrl: {}", manifest_server.url())));
    assert!(show_stdout.contains(&format!("sourceSha256: {sha256}")));
    assert!(show_stdout.contains("releaseVersion: 0.2.0"));
    assert!(show_stdout.contains("releaseChannel: stable"));
    assert!(show_stdout.contains("releaseSelectorKind: version"));
    assert!(show_stdout.contains("releaseSelectorValue: 0.2.0"));
    assert!(show_stdout.contains("description: stable manifest runtime"));
}

#[test]
fn runtime_install_from_official_release_installs_the_openclaw_package() {
    let root = TestDir::new("runtime-install-official-release");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(stdout(&install).contains("Installed runtime stable"));

    let install_root = runtime_install_root("stable", &env, &cwd).unwrap();
    let expected_binary = installed_openclaw_runtime_entrypoint(&install_root);
    assert_eq!(
        fs::read_to_string(&expected_binary).unwrap(),
        "#!/usr/bin/env node\nconsole.log('stable');\n"
    );

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("releaseVersion: 2026.3.24"));
    assert!(show_stdout.contains("releaseChannel: stable"));
    assert!(show_stdout.contains("sourceManifestUrl:"));
    assert!(show_stdout.contains(&format!("sourceIntegrity: {integrity}")));
    assert!(show_stdout.contains("sourceKind: installed"));

    let which = run_ocm(&cwd, &env, &["runtime", "which", "stable"]);
    assert!(which.status.success(), "{}", stderr(&which));
    assert_eq!(
        stdout(&which),
        format!("{}\n", path_string(&expected_binary))
    );

    let which_json = run_ocm(&cwd, &env, &["runtime", "which", "stable", "--json"]);
    assert!(which_json.status.success(), "{}", stderr(&which_json));
    let which_json_stdout = stdout(&which_json);
    assert!(which_json_stdout.contains("\"sourceKind\": \"installed\""));
    assert!(which_json_stdout.contains("\"releaseVersion\": \"2026.3.24\""));
    assert!(which_json_stdout.contains("\"releaseChannel\": \"stable\""));
    assert!(which_json_stdout.contains("\"releaseSelectorKind\": \"channel\""));
    assert!(which_json_stdout.contains("\"releaseSelectorValue\": \"stable\""));
    assert!(which_json_stdout.contains(&format!(
        "\"installRoot\": \"{}\"",
        path_string(&install_root)
    )));
}

#[test]
fn runtime_install_exposes_package_dependencies_to_openclaw_package_root() {
    let root = TestDir::new("runtime-install-package-deps");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball_with_dependencies(
        "#!/usr/bin/env node\nconsole.log('stable with deps');\n",
        "2026.4.25",
    );
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.4.25.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.4.25\"}},\"versions\":{{\"2026.4.25\":{{\"version\":\"2026.4.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.4.25\":\"2026-04-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "--version", "2026.4.25"],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let install_root = runtime_install_root("2026.4.25", &env, &cwd).unwrap();
    assert!(
        install_root
            .join("files/node_modules/chokidar/package.json")
            .exists()
    );
    assert!(
        install_root
            .join("files/node_modules/openclaw/node_modules/chokidar/package.json")
            .exists()
    );
    assert!(
        install_root
            .join("files/node_modules/openclaw/node_modules/readdirp/package.json")
            .exists()
    );
    assert!(
        install_root
            .join("files/node_modules/openclaw/node_modules/@scope/tool/package.json")
            .exists()
    );
}

#[test]
fn official_runtime_install_produces_a_runnable_env_binding() {
    let root = TestDir::new("runtime-install-official-runnable");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball(
        "#!/usr/bin/env node\nconsole.log(process.argv.slice(2).join(' '));\n",
    );
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "--", "status"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "status\n");
}

#[test]
fn official_runtime_install_stays_quiet_when_managed_node_fallback_is_available() {
    let root = TestDir::new("runtime-install-official-host-doctor");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    let empty_path = root.child("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    env.insert("PATH".to_string(), path_string(&empty_path));
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.15.0");

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));
    let output = stdout(&install);
    assert!(output.contains("Installed runtime stable"));
    assert!(!output.contains("healthy: true"));
    assert!(!output.contains("officialReleaseReady: true"));
    assert!(!output.contains("check: category=official-release"));
}

#[test]
fn runtime_install_rejects_non_canonical_names_for_official_releases() {
    let root = TestDir::new("runtime-install-official-canonical-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let packument = br#"{"dist-tags":{"latest":"2026.3.24"},"versions":{"2026.3.24":{"version":"2026.3.24","dist":{"tarball":"https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24.tgz"}}}}"#;
    let server = TestHttpServer::serve_bytes("/openclaw", "application/json", packument);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "latest", "--channel", "stable"],
    );
    assert_eq!(install.status.code(), Some(1));
    assert!(
        stderr(&install).contains(
            "official runtime installs use the canonical name \"stable\" for this selector"
        )
    );
}

#[test]
fn runtime_install_reuses_a_matching_official_runtime() {
    let root = TestDir::new("runtime-install-official-reuse");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
        2,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let first = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stdout(&first).contains("Installed runtime stable"));

    let second = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stdout(&second).contains("Using installed runtime stable"));
}

#[test]
fn concurrent_official_runtime_installs_reuse_the_published_runtime() {
    let root = TestDir::new("runtime-install-official-concurrent-reuse");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    let slow_npm = root.child("slow-npm");
    write_executable_script(
        &slow_npm,
        &format!(
            "#!/bin/sh\n/bin/sleep 1\nexec '{}' \"$@\"\n",
            root.child("fake-node-bin/npm").display()
        ),
    );
    env.insert(
        "OCM_INTERNAL_NPM_BIN".to_string(),
        slow_npm.display().to_string(),
    );

    let mut children = (0..2)
        .map(|_| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_ocm"));
            command
                .current_dir(&cwd)
                .args(["runtime", "install", "--channel", "stable"])
                .env_clear()
                .envs(&env)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.spawn().unwrap()
        })
        .collect::<Vec<_>>();

    let mut installed = 0;
    let mut reused = 0;
    for child in children.drain(..) {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        let output = stdout(&output);
        installed += usize::from(output.contains("Installed runtime stable"));
        reused += usize::from(output.contains("Using installed runtime stable"));
    }

    assert_eq!(installed, 1);
    assert_eq!(reused, 1);
}

#[test]
fn concurrent_official_runtime_installs_reject_changed_integrity() {
    let root = TestDir::new("runtime-install-official-concurrent-integrity");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let changed_integrity = sha512_integrity(b"different package bytes");
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = |integrity: &str| {
        format!(
            "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
            tarball_server.url(),
            integrity
        )
        .into_bytes()
    };
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![packument(&integrity), packument(&changed_integrity)],
    );
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    let slow_npm = root.child("slow-npm");
    write_executable_script(
        &slow_npm,
        &format!(
            "#!/bin/sh\n/bin/sleep 1\nexec '{}' \"$@\"\n",
            root.child("fake-node-bin/npm").display()
        ),
    );
    env.insert(
        "OCM_INTERNAL_NPM_BIN".to_string(),
        slow_npm.display().to_string(),
    );

    let mut first = Command::new(env!("CARGO_BIN_EXE_ocm"));
    first
        .current_dir(&cwd)
        .args(["runtime", "install", "--channel", "stable"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let first = first.spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while tarball_server.requests().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "first installer did not request the runtime tarball"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let second = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    let first = first.wait_with_output().unwrap();
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(second.status.code(), Some(1));
    assert!(stderr(&second).contains("runtime \"stable\" already exists"));
}

#[test]
fn runtime_install_refreshes_a_matching_official_runtime_with_bad_package_dep_layout() {
    let root = TestDir::new("runtime-install-official-bad-dep-layout");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball_with_dependencies(
        "#!/usr/bin/env node\nconsole.log('stable with deps');\n",
        "2026.4.25",
    );
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.4.25.tgz",
        "application/octet-stream",
        &tarball,
        2,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.4.25\"}},\"versions\":{{\"2026.4.25\":{{\"version\":\"2026.4.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.4.25\":\"2026-04-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let first = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "--version", "2026.4.25"],
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let install_root = runtime_install_root("2026.4.25", &env, &cwd).unwrap();
    let projected_dep =
        install_root.join("files/node_modules/openclaw/node_modules/chokidar/package.json");
    assert!(projected_dep.exists());
    fs::remove_dir_all(install_root.join("files/node_modules/openclaw/node_modules")).unwrap();

    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "2026.4.25", "--json"]);
    assert_eq!(verify.status.code(), Some(1));
    let value: Value = serde_json::from_str(&stdout(&verify)).unwrap();
    assert_eq!(value["healthy"], false);
    assert!(
        value["issue"]
            .as_str()
            .unwrap()
            .contains("OpenClaw package runtime dependency layout is missing")
    );

    let second = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "--version", "2026.4.25"],
    );
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stdout(&second).contains("Updated runtime 2026.4.25"));
    assert!(projected_dep.exists());
}

#[test]
fn runtime_install_refreshes_a_channel_runtime_when_the_published_release_moves() {
    let root = TestDir::new("runtime-install-official-refresh");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_tar = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable-1');\n");
    let stable_integrity = sha512_integrity(&stable_tar);
    let next_tar = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable-2');\n");
    let next_integrity = sha512_integrity(&next_tar);
    let first_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &stable_tar,
        2,
    );
    let next_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &next_tar,
        2,
    );
    let first_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        first_tarball_server.url(),
        stable_integrity
    );
    let second_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T10:00:00.000Z\"}}}}",
        first_tarball_server.url(),
        stable_integrity,
        next_tarball_server.url(),
        next_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![first_packument.into_bytes(), second_packument.into_bytes()],
    );
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let first = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stdout(&first).contains("Installed runtime stable"));

    let second = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stdout(&second).contains("Updated runtime stable"));

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("\"releaseVersion\": \"2026.3.25\""));
    assert!(show_stdout.contains("\"releaseSelectorValue\": \"stable\""));
}

#[test]
fn bound_official_runtime_is_reused_but_not_refreshed() {
    let root = TestDir::new("runtime-install-official-bound");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_tar = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable-1');\n");
    let stable_integrity = sha512_integrity(&stable_tar);
    let next_tar = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable-2');\n");
    let next_integrity = sha512_integrity(&next_tar);
    let stable_tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &stable_tar,
    );
    let next_tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &next_tar,
    );
    let stable_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}}}}",
        stable_tarball_server.url(),
        stable_integrity
    );
    let moved_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}}}}",
        stable_tarball_server.url(),
        stable_integrity,
        next_tarball_server.url(),
        next_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            stable_packument.as_bytes().to_vec(),
            stable_packument.into_bytes(),
            moved_packument.as_bytes().to_vec(),
            moved_packument.into_bytes(),
        ],
    );
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let reuse = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(reuse.status.success(), "{}", stderr(&reuse));
    assert!(stdout(&reuse).contains("Using installed runtime stable"));

    let refresh = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert_eq!(refresh.status.code(), Some(1));
    let error = stderr(&refresh);
    assert!(
        error.contains("runtime \"stable\" is still used"),
        "{error}"
    );
    assert!(error.contains("\"demo\""), "{error}");

    let create_on_moved_channel = run_ocm(
        &cwd,
        &env,
        &["env", "create", "second", "--channel", "stable"],
    );
    assert_eq!(create_on_moved_channel.status.code(), Some(1));
    assert!(
        stderr(&create_on_moved_channel).contains("runtime \"stable\" is still used"),
        "{}",
        stderr(&create_on_moved_channel)
    );
    assert!(next_tarball_server.requests().is_empty());

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("\"releaseVersion\": \"2026.3.24\""));
    let list = run_ocm(&cwd, &env, &["env", "list", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let environments: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert_eq!(environments.as_array().unwrap().len(), 1);
}

#[test]
fn runtime_releases_without_manifest_url_use_the_official_openclaw_source() {
    let root = TestDir::new("runtime-releases-official");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let packument = br#"{"dist-tags":{"latest":"2026.3.24","beta":"2026.3.24-beta.2"},"versions":{"2026.3.24":{"version":"2026.3.24","dist":{"tarball":"https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24.tgz"}},"2026.3.24-beta.2":{"version":"2026.3.24-beta.2","dist":{"tarball":"https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24-beta.2.tgz"}}}}"#;
    let server = TestHttpServer::serve_bytes("/openclaw", "application/json", packument);
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let output = run_ocm(&cwd, &env, &["runtime", "releases", "--channel", "stable"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let output = stdout(&output);
    assert!(output.contains("2026.3.24"));
    assert!(output.contains("channel=stable"));
    assert!(
        server.requests()[0]
            .to_ascii_lowercase()
            .contains("accept: application/vnd.npm.install-v1+json")
    );
}

#[test]
fn runtime_update_reuses_the_official_release_selector() {
    let root = TestDir::new("runtime-update-official-release");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_tar = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let updated_tar = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('updated');\n");
    let stable_integrity = sha512_integrity(&stable_tar);
    let updated_integrity = sha512_integrity(&updated_tar);
    let stable_tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &stable_tar,
    );
    let updated_tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &updated_tar,
    );
    let packument_v1 = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}}}}",
        stable_tarball_server.url(),
        stable_integrity
    );
    let packument_v2 = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.25\":\"2026-03-26T10:00:00.000Z\"}}}}",
        stable_tarball_server.url(),
        stable_integrity,
        updated_tarball_server.url(),
        updated_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![packument_v1.into_bytes(), packument_v2.into_bytes()],
    );
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "stable", "--channel", "stable"],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let update = run_ocm(&cwd, &env, &["runtime", "update", "stable"]);
    assert!(update.status.success(), "{}", stderr(&update));
    assert!(stdout(&update).contains("Updated runtime stable"));

    let install_root = runtime_install_root("stable", &env, &cwd).unwrap();
    let expected_binary = installed_openclaw_runtime_entrypoint(&install_root);
    assert_eq!(
        fs::read_to_string(&expected_binary).unwrap(),
        "#!/usr/bin/env node\nconsole.log('updated');\n"
    );

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("releaseVersion: 2026.3.25"));
}

#[test]
fn runtime_install_from_manifest_channel_selects_the_matching_release() {
    let root = TestDir::new("runtime-install-manifest-channel");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_body = b"stable-binary";
    let stable_digest_path = root.child("sha256/openclaw-stable");
    fs::create_dir_all(stable_digest_path.parent().unwrap()).unwrap();
    fs::write(&stable_digest_path, stable_body).unwrap();
    let stable_sha256 = file_sha256(&stable_digest_path).unwrap();

    let nightly_body = b"nightly-binary";
    let nightly_digest_path = root.child("sha256/openclaw-nightly");
    fs::write(&nightly_digest_path, nightly_body).unwrap();
    let nightly_sha256 = file_sha256(&nightly_digest_path).unwrap();

    let stable_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-stable",
        "application/octet-stream",
        stable_body,
    );
    let nightly_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-nightly",
        "application/octet-stream",
        nightly_body,
    );
    let manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}},{{\"version\":\"0.3.0-dev\",\"channel\":\"nightly\",\"url\":\"{}\",\"sha256\":\"{}\",\"description\":\"nightly manifest runtime\"}}]}}",
        stable_server.url(),
        stable_sha256,
        nightly_server.url(),
        nightly_sha256
    );
    let manifest_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body.as_bytes(),
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "nightly",
            "--manifest-url",
            &manifest_server.url(),
            "--channel",
            "nightly",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(stdout(&install).contains("Installed runtime nightly"));

    let install_root = runtime_install_root("nightly", &env, &cwd).unwrap();
    let expected_binary = install_root.join("files/openclaw-nightly");
    assert_eq!(fs::read(&expected_binary).unwrap(), nightly_body);

    let show = run_ocm(&cwd, &env, &["runtime", "show", "nightly"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("releaseVersion: 0.3.0-dev"));
    assert!(show_stdout.contains("releaseChannel: nightly"));
    assert!(show_stdout.contains("releaseSelectorKind: channel"));
    assert!(show_stdout.contains("releaseSelectorValue: nightly"));
    assert!(show_stdout.contains(&format!("sourceSha256: {nightly_sha256}")));
    assert!(show_stdout.contains("description: nightly manifest runtime"));
}

#[test]
fn official_runtime_install_uses_managed_node_when_npm_is_missing() {
    let root = TestDir::new("runtime-install-official-missing-npm");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.15.0");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    env.insert(
        "OCM_INTERNAL_NPM_BIN".to_string(),
        path_string(&root.child("missing-npm")),
    );
    env.insert(
        "OCM_TEST_REQUIRE_MANAGED_NODE_ON_PATH".to_string(),
        "1".to_string(),
    );

    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "--channel", "stable", "--json"],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(root.child("ocm-home/runtimes/stable").exists());

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "managed", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let run = run_ocm(&cwd, &env, &["env", "run", "managed", "--", "--version"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "stable\n");
}

#[test]
fn official_runtime_install_rejects_untrusted_managed_node_archive() {
    let root = TestDir::new("runtime-install-official-managed-node-integrity");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "20.11.0");
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.15.0");
    env.insert(
        "OCM_INTERNAL_MANAGED_NODE_ARCHIVE_SHA256".to_string(),
        "0".repeat(64),
    );
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(!install.status.success());
    assert!(
        stderr(&install).contains("runtime artifact sha256 mismatch"),
        "{}",
        stderr(&install)
    );
    let toolchain_root = root.child("ocm-home/toolchains/node");
    let installed_toolchains = fs::read_dir(toolchain_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("node-v"))
        .count();
    assert_eq!(installed_toolchains, 0);
}

#[test]
fn official_runtime_install_uses_managed_node_when_host_node_is_unsupported() {
    let root = TestDir::new("runtime-install-official-unsupported-node");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "23.11.0");
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.15.0");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "--channel", "stable", "--json"],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(root.child("ocm-home/runtimes/stable").exists());
}

#[test]
fn runtime_list_surfaces_release_metadata_for_manifest_installs() {
    let root = TestDir::new("runtime-list-release-metadata");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let artifact_body = b"release-runtime";
    let digest_path = root.child("sha256/openclaw-stable");
    fs::create_dir_all(digest_path.parent().unwrap()).unwrap();
    fs::write(&digest_path, artifact_body).unwrap();
    let sha256 = file_sha256(&digest_path).unwrap();

    let artifact_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-stable",
        "application/octet-stream",
        artifact_body,
    );
    let manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        artifact_server.url(),
        sha256
    );
    let manifest_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body.as_bytes(),
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--channel",
            "stable",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let list = run_ocm(&cwd, &env, &["runtime", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let output = stdout(&list);
    assert!(output.contains("stable"));
    assert!(output.contains("source=installed"));
    assert!(output.contains("release=0.2.0"));
    assert!(output.contains("channel=stable"));
}

#[test]
fn runtime_update_reinstalls_a_manifest_backed_runtime_with_a_new_version() {
    let root = TestDir::new("runtime-update-version");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_body = b"runtime-v0.2.0";
    let stable_digest_path = root.child("sha256/openclaw-0.2.0");
    fs::create_dir_all(stable_digest_path.parent().unwrap()).unwrap();
    fs::write(&stable_digest_path, stable_body).unwrap();
    let stable_sha256 = file_sha256(&stable_digest_path).unwrap();

    let next_body = b"runtime-v0.3.0";
    let next_digest_path = root.child("sha256/openclaw-0.3.0");
    fs::write(&next_digest_path, next_body).unwrap();
    let next_sha256 = file_sha256(&next_digest_path).unwrap();

    let stable_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.2.0",
        "application/octet-stream",
        stable_body,
    );
    let next_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.3.0",
        "application/octet-stream",
        next_body,
    );
    let manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}},{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        stable_server.url(),
        stable_sha256,
        next_server.url(),
        next_sha256
    );
    let manifest_server = TestHttpServer::serve_bytes_times(
        "/manifests/releases.json",
        "application/json",
        manifest_body.as_bytes(),
        2,
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--version",
            "0.2.0",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let update = run_ocm(
        &cwd,
        &env,
        &["runtime", "update", "stable", "--version", "0.3.0"],
    );
    assert!(update.status.success(), "{}", stderr(&update));
    assert!(stdout(&update).contains("Updated runtime stable"));

    let install_root = runtime_install_root("stable", &env, &cwd).unwrap();
    let expected_binary = install_root.join("files/openclaw-0.3.0");
    assert_eq!(fs::read(&expected_binary).unwrap(), next_body);

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("releaseVersion: 0.3.0"));
    assert!(output.contains("releaseChannel: stable"));
    assert!(output.contains(&format!("sourceSha256: {next_sha256}")));
}

#[test]
fn runtime_update_rejects_a_bound_runtime_without_fetching_the_replacement() {
    let root = TestDir::new("runtime-update-bound");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_body = b"runtime-v0.2.0";
    let stable_digest_path = root.child("sha256/openclaw-0.2.0");
    fs::create_dir_all(stable_digest_path.parent().unwrap()).unwrap();
    fs::write(&stable_digest_path, stable_body).unwrap();
    let stable_sha256 = file_sha256(&stable_digest_path).unwrap();
    let next_body = b"runtime-v0.3.0";
    let next_digest_path = root.child("sha256/openclaw-0.3.0");
    fs::write(&next_digest_path, next_body).unwrap();
    let next_sha256 = file_sha256(&next_digest_path).unwrap();
    let stable_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.2.0",
        "application/octet-stream",
        stable_body,
    );
    let next_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.3.0",
        "application/octet-stream",
        next_body,
    );
    let manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}},{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        stable_server.url(),
        stable_sha256,
        next_server.url(),
        next_sha256
    );
    let manifest_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body.as_bytes(),
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--version",
            "0.2.0",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let update = run_ocm(
        &cwd,
        &env,
        &["runtime", "update", "stable", "--version", "0.3.0"],
    );
    assert_eq!(update.status.code(), Some(1));
    let error = stderr(&update);
    assert!(
        error.contains("runtime \"stable\" is still used"),
        "{error}"
    );
    assert!(next_server.requests().is_empty());

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("\"releaseVersion\": \"0.2.0\""));
    let binary = runtime_install_root("stable", &env, &cwd).unwrap();
    assert_eq!(
        fs::read(binary.join("files/openclaw-0.2.0")).unwrap(),
        stable_body
    );
}

#[test]
fn runtime_update_without_arguments_reuses_the_stored_version_selector() {
    let root = TestDir::new("runtime-update-stored-version-selector");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_body = b"runtime-v0.2.0";
    let stable_digest_path = root.child("sha256/openclaw-0.2.0");
    fs::create_dir_all(stable_digest_path.parent().unwrap()).unwrap();
    fs::write(&stable_digest_path, stable_body).unwrap();
    let stable_sha256 = file_sha256(&stable_digest_path).unwrap();

    let next_body = b"runtime-v0.3.0";
    let next_digest_path = root.child("sha256/openclaw-0.3.0");
    fs::write(&next_digest_path, next_body).unwrap();
    let next_sha256 = file_sha256(&next_digest_path).unwrap();

    let stable_server = TestHttpServer::serve_bytes_times(
        "/artifacts/openclaw-0.2.0",
        "application/octet-stream",
        stable_body,
        2,
    );
    let next_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.3.0",
        "application/octet-stream",
        next_body,
    );
    let first_manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        stable_server.url(),
        stable_sha256
    );
    let second_manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}},{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        stable_server.url(),
        stable_sha256,
        next_server.url(),
        next_sha256
    );
    let manifest_server = TestHttpServer::serve_bytes_sequence(
        "/manifests/releases.json",
        "application/json",
        vec![
            first_manifest_body.into_bytes(),
            second_manifest_body.into_bytes(),
        ],
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--version",
            "0.2.0",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let update = run_ocm(&cwd, &env, &["runtime", "update", "stable"]);
    assert!(update.status.success(), "{}", stderr(&update));
    assert!(stdout(&update).contains("Updated runtime stable"));

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("releaseVersion: 0.2.0"));
    assert!(output.contains("releaseSelectorKind: version"));
    assert!(output.contains("releaseSelectorValue: 0.2.0"));
}

#[test]
fn runtime_update_without_arguments_reuses_the_stored_channel_selector() {
    let root = TestDir::new("runtime-update-stored-channel-selector");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let first_body = b"runtime-v0.2.0";
    let first_digest_path = root.child("sha256/openclaw-0.2.0");
    fs::create_dir_all(first_digest_path.parent().unwrap()).unwrap();
    fs::write(&first_digest_path, first_body).unwrap();
    let first_sha256 = file_sha256(&first_digest_path).unwrap();

    let second_body = b"runtime-v0.3.0";
    let second_digest_path = root.child("sha256/openclaw-0.3.0");
    fs::write(&second_digest_path, second_body).unwrap();
    let second_sha256 = file_sha256(&second_digest_path).unwrap();

    let first_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.2.0",
        "application/octet-stream",
        first_body,
    );
    let second_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-0.3.0",
        "application/octet-stream",
        second_body,
    );
    let first_manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        first_server.url(),
        first_sha256
    );
    let second_manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        second_server.url(),
        second_sha256
    );
    let manifest_server = TestHttpServer::serve_bytes_sequence(
        "/manifests/releases.json",
        "application/json",
        vec![
            first_manifest_body.into_bytes(),
            second_manifest_body.into_bytes(),
        ],
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--channel",
            "stable",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let update = run_ocm(&cwd, &env, &["runtime", "update", "stable"]);
    assert!(update.status.success(), "{}", stderr(&update));
    assert!(stdout(&update).contains("Updated runtime stable"));

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("releaseVersion: 0.3.0"));
    assert!(output.contains("releaseChannel: stable"));
    assert!(output.contains("releaseSelectorKind: channel"));
    assert!(output.contains("releaseSelectorValue: stable"));
}

#[test]
fn runtime_update_all_uses_stored_selectors_and_skips_registered_runtimes() {
    let root = TestDir::new("runtime-update-all");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let external_path = bin_dir.join("external");
    write_executable_script(&external_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "external", "--path", "./bin/external"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let pinned_body = b"runtime-v0.2.0";
    let pinned_digest_path = root.child("sha256/openclaw-0.2.0");
    fs::create_dir_all(pinned_digest_path.parent().unwrap()).unwrap();
    fs::write(&pinned_digest_path, pinned_body).unwrap();
    let pinned_sha256 = file_sha256(&pinned_digest_path).unwrap();

    let pinned_next_body = b"runtime-v0.3.0";
    let pinned_next_digest_path = root.child("sha256/openclaw-0.3.0");
    fs::write(&pinned_next_digest_path, pinned_next_body).unwrap();
    let pinned_next_sha256 = file_sha256(&pinned_next_digest_path).unwrap();

    let pinned_server = TestHttpServer::serve_bytes_times(
        "/artifacts/openclaw-0.2.0",
        "application/octet-stream",
        pinned_body,
        3,
    );
    let pinned_next_server = TestHttpServer::serve_bytes_times(
        "/artifacts/openclaw-0.3.0",
        "application/octet-stream",
        pinned_next_body,
        2,
    );
    let pinned_manifest_server = TestHttpServer::serve_bytes_sequence(
        "/manifests/pinned.json",
        "application/json",
        vec![
            format!(
                "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
                pinned_server.url(),
                pinned_sha256
            )
            .into_bytes(),
            format!(
                "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}},{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
                pinned_server.url(),
                pinned_sha256,
                pinned_next_server.url(),
                pinned_next_sha256
            )
            .into_bytes(),
            format!(
                "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}},{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
                pinned_server.url(),
                pinned_sha256,
                pinned_next_server.url(),
                pinned_next_sha256
            )
            .into_bytes(),
        ],
    );

    let tracked_first_body = b"runtime-nightly-v0.2.0";
    let tracked_first_digest_path = root.child("sha256/openclaw-nightly-0.2.0");
    fs::write(&tracked_first_digest_path, tracked_first_body).unwrap();
    let tracked_first_sha256 = file_sha256(&tracked_first_digest_path).unwrap();

    let tracked_second_body = b"runtime-nightly-v0.3.0";
    let tracked_second_digest_path = root.child("sha256/openclaw-nightly-0.3.0");
    fs::write(&tracked_second_digest_path, tracked_second_body).unwrap();
    let tracked_second_sha256 = file_sha256(&tracked_second_digest_path).unwrap();

    let tracked_first_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-nightly-0.2.0",
        "application/octet-stream",
        tracked_first_body,
    );
    let tracked_second_server = TestHttpServer::serve_bytes_times(
        "/artifacts/openclaw-nightly-0.3.0",
        "application/octet-stream",
        tracked_second_body,
        2,
    );
    let tracked_manifest_server = TestHttpServer::serve_bytes_sequence(
        "/manifests/nightly.json",
        "application/json",
        vec![
            format!(
                "{{\"releases\":[{{\"version\":\"0.2.0-dev\",\"channel\":\"nightly\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
                tracked_first_server.url(),
                tracked_first_sha256
            )
            .into_bytes(),
            format!(
                "{{\"releases\":[{{\"version\":\"0.3.0-dev\",\"channel\":\"nightly\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
                tracked_second_server.url(),
                tracked_second_sha256
            )
            .into_bytes(),
            format!(
                "{{\"releases\":[{{\"version\":\"0.3.0-dev\",\"channel\":\"nightly\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
                tracked_second_server.url(),
                tracked_second_sha256
            )
            .into_bytes(),
        ],
    );

    let install_pinned = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable-pinned",
            "--manifest-url",
            &pinned_manifest_server.url(),
            "--version",
            "0.2.0",
        ],
    );
    assert!(
        install_pinned.status.success(),
        "{}",
        stderr(&install_pinned)
    );

    let install_tracked = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "nightly",
            "--manifest-url",
            &tracked_manifest_server.url(),
            "--channel",
            "nightly",
        ],
    );
    assert!(
        install_tracked.status.success(),
        "{}",
        stderr(&install_tracked)
    );
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "nightly"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let update = run_ocm(&cwd, &env, &["runtime", "update", "--all", "--json"]);
    assert_eq!(update.status.code(), Some(1));
    let value: Value = serde_json::from_str(&stdout(&update)).unwrap();
    assert_eq!(value["count"], 3);
    assert_eq!(value["updated"], 1);
    assert_eq!(value["skipped"], 1);
    assert_eq!(value["failed"], 1);
    let array = value["results"].as_array().unwrap();
    assert_eq!(array.len(), 3);
    assert!(array.iter().any(|item| {
        item["name"] == "external"
            && item["outcome"] == "skipped"
            && item["issue"]
                .as_str()
                .unwrap()
                .contains("not backed by a release manifest")
    }));
    assert!(array.iter().any(|item| {
        item["name"] == "stable-pinned"
            && item["outcome"] == "updated"
            && item["releaseVersion"] == "0.2.0"
    }));
    assert!(array.iter().any(|item| {
        item["name"] == "nightly"
            && item["outcome"] == "failed"
            && item["releaseVersion"] == "0.2.0-dev"
            && item["releaseChannel"] == "nightly"
            && item["issue"]
                .as_str()
                .unwrap()
                .contains("runtime \"nightly\" is still used")
    }));
    assert!(tracked_second_server.requests().is_empty());

    let update_plain = run_ocm(&cwd, &env, &["runtime", "update", "--all"]);
    assert_eq!(update_plain.status.code(), Some(1));
    let plain_output = stdout(&update_plain);
    assert!(plain_output.contains("Runtime update summary: total=3 updated=1 skipped=1 failed=1"));
    assert!(plain_output.contains("external  outcome=skipped"));
    assert!(plain_output.contains("stable-pinned  outcome=updated"));
    assert!(plain_output.contains("nightly  outcome=failed"));
    assert!(tracked_second_server.requests().is_empty());
}

#[test]
fn runtime_install_from_url_cleans_up_failed_install_roots_for_retry() {
    let root = TestDir::new("runtime-install-url-retry");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let missing_server = TestHttpServer::serve_bytes(
        "/releases/openclaw-nightly",
        "application/octet-stream",
        b"downloaded-runtime",
    );
    let env = ocm_env(&root);

    let failed_install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "nightly",
            "--url",
            &format!("{}-missing", missing_server.url()),
        ],
    );
    assert_eq!(failed_install.status.code(), Some(1));
    assert!(stderr(&failed_install).contains("failed to download runtime URL"));

    let install_root = runtime_install_root("nightly", &env, &cwd).unwrap();
    assert!(!install_root.exists());

    let retry_server = TestHttpServer::serve_bytes(
        "/releases/openclaw-nightly",
        "application/octet-stream",
        b"downloaded-runtime",
    );
    let retry_install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "nightly",
            "--url",
            &retry_server.url(),
        ],
    );
    assert!(retry_install.status.success(), "{}", stderr(&retry_install));
    assert!(stdout(&retry_install).contains("Installed runtime nightly"));
}

#[test]
fn release_backed_runtime_install_requires_sha256_before_download() {
    let root = TestDir::new("runtime-install-manifest-integrity-required");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let artifact_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-stable",
        "application/octet-stream",
        b"runtime",
    );
    let manifest = format!(
        "{{\"releases\":[{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\"}}]}}",
        artifact_server.url()
    );
    let manifest_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest.as_bytes(),
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--version",
            "0.3.0",
        ],
    );
    assert!(!install.status.success());
    assert!(stderr(&install).contains("missing required sha256 integrity"));
    assert!(artifact_server.requests().is_empty());
}

#[test]
fn official_runtime_install_requires_sha512_integrity_before_download() {
    let root = TestDir::new("runtime-install-official-integrity-required");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        b"runtime",
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\"}}}}}}}}",
        tarball_server.url()
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(!install.status.success());
    assert!(stderr(&install).contains("missing required sha512 integrity"));
    assert!(tarball_server.requests().is_empty());
}

#[test]
fn runtime_install_force_replaces_an_existing_runtime_definition() {
    let root = TestDir::new("runtime-install-force");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    let source_dir = cwd.join("downloads");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&source_dir).unwrap();
    let external_path = bin_dir.join("stable");
    let managed_source_path = source_dir.join("openclaw");
    write_executable_script(&external_path, "#!/bin/sh\nexit 0\n");
    write_executable_script(&managed_source_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let duplicate_install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./downloads/openclaw",
        ],
    );
    assert_eq!(duplicate_install.status.code(), Some(1));
    assert!(stderr(&duplicate_install).contains("runtime \"stable\" already exists"));

    let force_install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./downloads/openclaw",
            "--force",
        ],
    );
    assert!(force_install.status.success(), "{}", stderr(&force_install));
    assert!(stdout(&force_install).contains("Installed runtime stable"));

    let install_root = runtime_install_root("stable", &env, &cwd).unwrap();
    let expected_binary = install_root.join("files/openclaw");
    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("\"sourceKind\": \"installed\""));
    assert!(show_stdout.contains(&format!(
        "\"binaryPath\": \"{}\"",
        path_string(&expected_binary)
    )));
}

#[test]
fn runtime_install_force_rejects_replacing_a_bound_runtime() {
    let root = TestDir::new("runtime-install-force-bound");
    let cwd = root.child("workspace");
    let source_dir = cwd.join("downloads");
    fs::create_dir_all(&source_dir).unwrap();
    let original_path = source_dir.join("openclaw-original");
    let replacement_path = source_dir.join("openclaw-replacement");
    write_executable_script(&original_path, "#!/bin/sh\nprintf 'original\\n'\n");
    write_executable_script(&replacement_path, "#!/bin/sh\nprintf 'replacement\\n'\n");
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./downloads/openclaw-original",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let replace = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./downloads/openclaw-replacement",
            "--force",
        ],
    );
    assert_eq!(replace.status.code(), Some(1));
    let error = stderr(&replace);
    assert!(
        error.contains("runtime \"stable\" is still used"),
        "{error}"
    );

    let install_root = runtime_install_root("stable", &env, &cwd).unwrap();
    assert_eq!(
        fs::read_to_string(install_root.join("files/openclaw-original")).unwrap(),
        "#!/bin/sh\nprintf 'original\\n'\n"
    );
    assert!(!install_root.join("files/openclaw-replacement").exists());
}

#[test]
fn runtime_install_force_preserves_existing_runtime_when_replacement_fails() {
    let root = TestDir::new("runtime-install-force-rollback");
    let cwd = root.child("workspace");
    let source_dir = cwd.join("downloads");
    fs::create_dir_all(&source_dir).unwrap();
    let source_path = source_dir.join("openclaw");
    write_executable_script(&source_path, "#!/bin/sh\nprintf 'working\\n'\n");
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./downloads/openclaw",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let replacement = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-stable",
        "application/octet-stream",
        b"broken replacement",
    );
    let manifest = format!(
        "{{\"releases\":[{{\"version\":\"0.3.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        replacement.url(),
        "0".repeat(64)
    );
    let manifest_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest.as_bytes(),
    );
    let failed = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--version",
            "0.3.0",
            "--force",
        ],
    );
    assert!(!failed.status.success());
    assert!(stderr(&failed).contains("runtime artifact sha256 mismatch"));

    let show = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("\"sourcePath\""));
    let binary = runtime_install_root("stable", &env, &cwd)
        .unwrap()
        .join("files/openclaw");
    assert_eq!(
        fs::read_to_string(binary).unwrap(),
        "#!/bin/sh\nprintf 'working\\n'\n"
    );

    let runtime_dir = root.child("ocm-home/runtimes");
    let residues = fs::read_dir(runtime_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".stage-") || name.contains(".backup-"))
        .collect::<Vec<_>>();
    assert!(residues.is_empty(), "{residues:?}");
}

#[test]
fn runtime_releases_lists_manifest_entries() {
    let root = TestDir::new("runtime-releases");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let manifest_body = b"{\"releases\":[{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"https://example.test/openclaw-stable\",\"sha256\":\"abc123\"},{\"version\":\"0.3.0-dev\",\"channel\":\"nightly\",\"url\":\"https://example.test/openclaw-nightly\"}]}";
    let server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body,
    );
    let env = ocm_env(&root);

    let list = run_ocm(
        &cwd,
        &env,
        &["runtime", "releases", "--manifest-url", &server.url()],
    );
    assert!(list.status.success(), "{}", stderr(&list));
    let output = stdout(&list);
    assert!(output.contains("0.2.0"));
    assert!(output.contains("channel=stable"));
    assert!(output.contains("sha256=abc123"));
    assert!(output.contains("0.3.0-dev"));

    let raw_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body,
    );
    let raw = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "releases",
            "--manifest-url",
            &raw_server.url(),
            "--raw",
        ],
    );
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert!(stdout(&raw).contains("0.2.0"));

    let color_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body,
    );
    let color = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "releases",
            "--manifest-url",
            &color_server.url(),
            "--color",
            "never",
        ],
    );
    assert!(color.status.success(), "{}", stderr(&color));
    assert!(stdout(&color).contains("0.2.0"));

    let mixed = run_ocm(&cwd, &env, &["runtime", "releases", "--json", "--raw"]);
    assert_eq!(mixed.status.code(), Some(1));
    assert!(stderr(&mixed).contains("runtime releases accepts only one of --json or --raw"));

    let json_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body,
    );
    let json = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "releases",
            "--manifest-url",
            &json_server.url(),
            "--json",
        ],
    );
    assert!(json.status.success(), "{}", stderr(&json));
    let json_output = stdout(&json);
    assert!(json_output.contains("\"version\": \"0.2.0\""));
    assert!(json_output.contains("\"channel\": \"stable\""));
}

#[test]
fn runtime_releases_can_filter_by_version_or_channel() {
    let root = TestDir::new("runtime-releases-filtered");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let manifest_body = b"{\"releases\":[{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"https://example.test/openclaw-stable\",\"sha256\":\"abc123\"},{\"version\":\"0.3.0-dev\",\"channel\":\"nightly\",\"url\":\"https://example.test/openclaw-nightly\"}]}";
    let env = ocm_env(&root);

    let version_server = TestHttpServer::serve_bytes(
        "/manifests/releases-version.json",
        "application/json",
        manifest_body,
    );
    let by_version = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "releases",
            "--manifest-url",
            &version_server.url(),
            "--version",
            "0.2.0",
        ],
    );
    assert!(by_version.status.success(), "{}", stderr(&by_version));
    let version_output = stdout(&by_version);
    assert!(version_output.contains("0.2.0"));
    assert!(version_output.contains("channel=stable"));
    assert!(!version_output.contains("0.3.0-dev"));

    let channel_server = TestHttpServer::serve_bytes(
        "/manifests/releases-channel.json",
        "application/json",
        manifest_body,
    );
    let by_channel = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "releases",
            "--manifest-url",
            &channel_server.url(),
            "--channel",
            "nightly",
            "--json",
        ],
    );
    assert!(by_channel.status.success(), "{}", stderr(&by_channel));
    let value: Value = serde_json::from_str(&stdout(&by_channel)).unwrap();
    let array = value.as_array().unwrap();
    assert_eq!(array.len(), 1);
    assert_eq!(array[0]["version"], "0.3.0-dev");
    assert_eq!(array[0]["channel"], "nightly");
}
