#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use serde::Serialize;
use sha2::Digest;
use sha2::Sha512;
use tar::{Builder, Header};
use tempfile::NamedTempFile;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub struct TestDir {
    path: PathBuf,
}

pub struct TestHttpServer {
    addr: String,
    path: String,
    served: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    request_limit: usize,
    handle: Option<JoinHandle<()>>,
}

impl TestDir {
    pub fn new(label: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join("ocm-tests")
            .join(format!("{label}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn child(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.path.join(relative)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl TestHttpServer {
    pub fn serve_bytes(path: &str, content_type: &str, body: &[u8]) -> Self {
        Self::serve_bytes_times(path, content_type, body, 1)
    }

    pub fn serve_bytes_with_headers(
        path: &str,
        content_type: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> Self {
        Self::serve_bytes_sequence_with_headers(path, content_type, vec![body.to_vec()], headers)
    }

    pub fn serve_bytes_times(
        path: &str,
        content_type: &str,
        body: &[u8],
        request_limit: usize,
    ) -> Self {
        Self::serve_bytes_sequence(
            path,
            content_type,
            vec![body.to_vec(); request_limit.max(1)],
        )
    }

    pub fn serve_bytes_sequence(path: &str, content_type: &str, bodies: Vec<Vec<u8>>) -> Self {
        Self::serve_bytes_sequence_with_headers(path, content_type, bodies, &[])
    }

    fn serve_bytes_sequence_with_headers(
        path: &str,
        content_type: &str,
        bodies: Vec<Vec<u8>>,
        headers: &[(&str, &str)],
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let addr_string = format!("127.0.0.1:{}", addr.port());
        let path_string = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let response_path = path_string.clone();
        let response_type = content_type.to_string();
        let response_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect::<String>();
        let response_bodies = if bodies.is_empty() {
            vec![Vec::new()]
        } else {
            bodies
        };
        let request_limit = response_bodies.len();
        let served = Arc::new(AtomicUsize::new(0));
        let served_flag = Arc::clone(&served);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_log = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for response_body in response_bodies {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let request_text = String::from_utf8_lossy(&request);
                request_log.lock().unwrap().push(request_text.to_string());
                let status_line = if request_text.starts_with(&format!("GET {response_path} ")) {
                    "HTTP/1.1 200 OK"
                } else {
                    "HTTP/1.1 404 Not Found"
                };
                let body = if status_line.ends_with("200 OK") {
                    response_body
                } else {
                    b"not found".to_vec()
                };
                let response = format!(
                    "{status_line}\r\nContent-Length: {}\r\nContent-Type: {}\r\n{response_headers}Connection: close\r\n\r\n",
                    body.len(),
                    response_type
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
                served_flag.fetch_add(1, Ordering::SeqCst);
            }
        });

        Self {
            addr: addr_string,
            path: path_string,
            served,
            requests,
            request_limit,
            handle: Some(handle),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}{}", self.addr, self.path)
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        if let Ok(addr) = self.addr.parse::<SocketAddr>() {
            let remaining = self
                .request_limit
                .saturating_sub(self.served.load(Ordering::SeqCst));

            for _ in 0..remaining {
                if self.served.load(Ordering::SeqCst) >= self.request_limit {
                    break;
                }

                let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100))
                else {
                    break;
                };
                let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                let _ = write!(
                    stream,
                    "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    self.path, self.addr
                );
                let _ = stream.flush();
            }
        }

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn base_env(home: &Path) -> BTreeMap<String, String> {
    fs::create_dir_all(home).unwrap();

    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), path_string(home));
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    env
}

pub fn ocm_env(root: &TestDir) -> BTreeMap<String, String> {
    let home = root.child("home");
    let ocm_home = root.child("ocm-home");
    fs::create_dir_all(&ocm_home).unwrap();

    let mut env = base_env(&home);
    env.insert("OCM_HOME".to_string(), path_string(&ocm_home));
    env.insert(
        "OCM_INTERNAL_SKIP_SERVICE_READINESS".to_string(),
        "1".to_string(),
    );
    env
}

pub fn managed_service_label(_env: &BTreeMap<String, String>, _cwd: &Path, _name: &str) -> String {
    "ai.openclaw.ocm".to_string()
}

pub fn managed_service_definition_path(
    env: &BTreeMap<String, String>,
    cwd: &Path,
    name: &str,
) -> PathBuf {
    let label = managed_service_label(env, cwd, name);
    let home = PathBuf::from(
        env.get("HOME")
            .cloned()
            .unwrap_or_else(|| path_string(&cwd.join("home"))),
    );
    if env
        .get("OCM_INTERNAL_SERVICE_MANAGER")
        .is_some_and(|value| value.contains("systemd"))
    {
        home.join(".config")
            .join("systemd")
            .join("user")
            .join(format!("{label}.service"))
    } else {
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{label}.plist"))
    }
}

pub fn write_text(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// Writes a complete JSON document by atomically replacing the destination path.
pub fn write_json_replacing_path<T: Serialize>(path: &Path, value: &T) {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    let mut temp = NamedTempFile::new_in(parent).unwrap();
    serde_json::to_writer(&mut temp, value).unwrap();
    temp.write_all(b"\n").unwrap();
    temp.persist(path).unwrap();
}

pub fn write_executable_script(path: &Path, contents: &str) {
    write_text(path, contents);
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

pub fn install_fake_launchctl(root: &TestDir, env: &mut BTreeMap<String, String>) {
    let bin_dir = root.child("fake-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let log_path = root.child("launchctl.log");
    let print_path = root.child("launchctl-print.txt");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\ncase \"$1\" in\n  bootstrap)\n    printf 'state = running\\npid = 23613\\n' > \"{}\"\n    exit 0\n    ;;\n  bootout|unload)\n    /bin/rm -f \"{}\"\n    exit 0\n    ;;\n  print)\n    if [ -f \"{}\" ]; then\n      /bin/cat \"{}\"\n      exit 0\n    fi\n    printf 'Could not find service \"%s\" in domain for user gui\\n' \"$2\" >&2\n    exit 1\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
        path_string(&log_path),
        path_string(&print_path),
        path_string(&print_path),
        path_string(&print_path),
        path_string(&print_path),
    );
    write_executable_script(&bin_dir.join("launchctl"), &script);

    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(&bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(&bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
    env.insert(
        "OCM_INTERNAL_LAUNCHCTL_BIN".to_string(),
        path_string(&bin_dir.join("launchctl")),
    );
}

pub fn install_fake_systemd_tools(root: &TestDir, env: &mut BTreeMap<String, String>) {
    let bin_dir = root.child("fake-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let log_path = root.child("systemctl.log");
    let journal_log_path = root.child("journalctl.log");
    let systemctl_script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$1\" = \"--user\" ] && [ \"$2\" = \"show\" ]; then\n  unit=\"$3\"\n  home=\"${{HOME:-$PWD}}\"\n  unit_path=\"$home/.config/systemd/user/$unit.service\"\n  if [ -f \"$unit_path\" ]; then\n    printf 'LoadState=loaded\\nUnitFileState=enabled\\nActiveState=active\\nSubState=running\\nMainPID=4242\\nFragmentPath=%s\\n' \"$unit_path\"\n    exit 0\n  fi\n  printf 'Unit %s could not be found\\n' \"$unit\" >&2\n  exit 1\nfi\nexit 0\n",
        path_string(&log_path)
    );
    let journalctl_script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nprintf 'gateway ok\\n'\n",
        path_string(&journal_log_path)
    );
    write_executable_script(&bin_dir.join("systemctl"), &systemctl_script);
    write_executable_script(&bin_dir.join("journalctl"), &journalctl_script);

    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(&bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(&bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "systemd-user".to_string(),
    );
    env.insert(
        "OCM_INTERNAL_SYSTEMCTL_BIN".to_string(),
        path_string(&bin_dir.join("systemctl")),
    );
    env.insert(
        "OCM_INTERNAL_JOURNALCTL_BIN".to_string(),
        path_string(&bin_dir.join("journalctl")),
    );
}

pub fn install_fake_service_manager(root: &TestDir, env: &mut BTreeMap<String, String>) {
    if cfg!(target_os = "macos") {
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "launchd".to_string(),
        );
        install_fake_launchctl(root, env);
    } else {
        install_fake_systemd_tools(root, env);
    }
}

pub fn install_fake_git_package_manager(
    root: &TestDir,
    env: &mut BTreeMap<String, String>,
    manager: &str,
) -> PathBuf {
    let bin_dir = root.child("fake-host-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let log_path = root.child(format!("{manager}.log"));
    let git_script =
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf 'git version 2.51.0\\n'\n  exit 0\nfi\nprintf 'fake git %s\\n' \"$*\"\n"
            .to_string();
    let manager_script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$1\" = \"--version\" ]; then\n  printf '{} version 1.0.0\\n'\n  exit 0\nfi\ncase \"$1\" in\n  update)\n    exit 0\n    ;;\n  install)\n    ;;\n  add)\n    ;;\n  *)\n    echo 'unexpected fake {} command: '$* >&2\n    exit 1\n    ;;\nesac\n/bin/cat > \"{}/git\" <<'EOF'\n{}EOF\n/bin/chmod 755 \"{}/git\"\n",
        path_string(&log_path),
        manager,
        manager,
        path_string(&bin_dir),
        git_script,
        path_string(&bin_dir),
    );
    write_executable_script(&bin_dir.join(manager), &manager_script);

    let sudo_script =
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf 'sudo 1.0.0\\n'\n  exit 0\nfi\nexec \"$@\"\n"
            .to_string();
    write_executable_script(&bin_dir.join("sudo"), &sudo_script);

    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(&bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(&bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
    log_path
}

pub fn install_fake_node_and_npm(
    root: &TestDir,
    env: &mut BTreeMap<String, String>,
    node_version: &str,
) {
    let bin_dir = root.child("fake-node-bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let node_script = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'v{}\n'
  exit 0
fi
script="$1"
shift
if [ -n "$script" ] && grep -q "process\.argv\.slice(2)\.join(' ')" "$script"; then
  printf '%s\n' "$*"
  exit 0
fi
literal=$(sed -n "s/.*console\.log('\(.*\)');.*/\1/p" "$script" | head -n 1)
if [ -n "$literal" ]; then
  printf '%s\n' "$literal"
  exit 0
fi
printf 'fake node run %s %s\n' "$script" "$*"
"#,
        node_version
    );
    write_executable_script(&bin_dir.join("node"), &node_script);

    let npm_script = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '10.0.0\n'
  exit 0
fi

prefix=""
archive=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      shift
      prefix="$1"
      ;;
    install|--omit=dev|--no-save|--package-lock=false)
      ;;
    *)
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
if grep -q '"chokidar"' "$prefix/node_modules/openclaw/package.json"; then
  mkdir -p "$prefix/node_modules/chokidar"
  mkdir -p "$prefix/node_modules/readdirp"
  mkdir -p "$prefix/node_modules/@scope/tool"
  printf '{"name":"chokidar","version":"5.0.0"}\n' > "$prefix/node_modules/chokidar/package.json"
  printf '{"name":"readdirp","version":"4.1.2"}\n' > "$prefix/node_modules/readdirp/package.json"
  printf '{"name":"@scope/tool","version":"1.0.0"}\n' > "$prefix/node_modules/@scope/tool/package.json"
fi
"#;
    write_executable_script(&bin_dir.join("npm"), npm_script);

    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(&bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(&bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
}

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

pub fn openclaw_package_tarball(script_body: &str, version: &str) -> Vec<u8> {
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
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

pub fn fake_managed_node_archive(version: &str) -> Vec<u8> {
    let (suffix, node_relative_path, npm_cli_relative_path, archive_kind) =
        managed_node_archive_layout();
    let root = format!("node-v{version}-{suffix}");
    let node_script = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'v{}\n'
  exit 0
fi
script="$1"
shift
case "$script" in
  *npm-cli.js)
    if [ "${{OCM_TEST_REQUIRE_MANAGED_NODE_ON_PATH:-}}" = "1" ]; then
      resolved_node="$(command -v node || true)"
      if [ "$resolved_node" != "$0" ]; then
        echo "managed npm lifecycle PATH resolved node to ${{resolved_node:-missing}}, expected $0" >&2
        exit 1
      fi
    fi
    prefix=""
    archive=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --prefix)
          shift
          prefix="$1"
          ;;
        install|--omit=dev|--no-save|--package-lock=false)
          ;;
        *)
          archive="$1"
          ;;
      esac
      shift
    done

    if [ -z "$prefix" ] || [ -z "$archive" ]; then
      echo "fake managed node expected --prefix and archive path" >&2
      exit 1
    fi

    /bin/mkdir -p "$prefix/node_modules/openclaw"
    /bin/cat > "$prefix/node_modules/openclaw/openclaw.mjs" <<'EOF'
#!/usr/bin/env node
console.log('stable');
EOF
    /bin/cat > "$prefix/node_modules/openclaw/package.json" <<'EOF'
{{"name":"openclaw","version":"2026.3.24","bin":{{"openclaw":"openclaw.mjs"}}}}
EOF
    /bin/chmod 755 "$prefix/node_modules/openclaw/openclaw.mjs"
    exit 0
    ;;
esac
if [ "${{OCM_TEST_REQUIRE_MANAGED_NODE_ON_PATH:-}}" = "1" ]; then
  resolved_node="$(command -v node || true)"
  if [ "$resolved_node" != "$0" ]; then
    echo "managed OpenClaw PATH resolved node to ${{resolved_node:-missing}}, expected $0" >&2
    exit 1
  fi
fi
if [ -n "$script" ] && /usr/bin/grep -q "process\.argv\.slice(2)\.join(' ')" "$script"; then
  printf '%s\n' "$*"
  exit 0
fi
literal=$(/usr/bin/sed -n "s/.*console\.log('\(.*\)');.*/\1/p" "$script" | /usr/bin/head -n 1)
if [ -n "$literal" ]; then
  printf '%s\n' "$literal"
  exit 0
fi
printf 'fake managed node run %s %s\n' "$script" "$*"
"#,
        version
    );
    let npm_cli = b"// fake npm cli entrypoint for managed-node tests\n";

    match archive_kind {
        ManagedNodeArchiveKind::TarGz => {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            {
                let mut builder = Builder::new(&mut encoder);
                append_tar_file(
                    &mut builder,
                    &format!("{root}/{node_relative_path}"),
                    node_script.as_bytes(),
                    0o755,
                );
                append_tar_file(
                    &mut builder,
                    &format!("{root}/{npm_cli_relative_path}"),
                    npm_cli,
                    0o644,
                );
                builder.finish().unwrap();
            }
            encoder.finish().unwrap()
        }
        ManagedNodeArchiveKind::Zip => {
            let cursor = std::io::Cursor::new(Vec::new());
            let mut writer = ZipWriter::new(cursor);
            let node_options = SimpleFileOptions::default().unix_permissions(0o755);
            let file_options = SimpleFileOptions::default().unix_permissions(0o644);
            writer
                .start_file(format!("{root}/{node_relative_path}"), node_options)
                .unwrap();
            writer.write_all(node_script.as_bytes()).unwrap();
            writer
                .start_file(format!("{root}/{npm_cli_relative_path}"), file_options)
                .unwrap();
            writer.write_all(npm_cli).unwrap();
            writer.finish().unwrap().into_inner()
        }
    }
}

pub fn install_fake_managed_node_archive(
    _root: &TestDir,
    env: &mut BTreeMap<String, String>,
    version: &str,
) -> TestHttpServer {
    let archive = fake_managed_node_archive(version);
    let archive_sha256 = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(&archive))
    };
    let server = TestHttpServer::serve_bytes(
        "/managed-node-toolchain",
        "application/octet-stream",
        &archive,
    );
    env.insert(
        "OCM_INTERNAL_MANAGED_NODE_ARCHIVE_URL".to_string(),
        server.url(),
    );
    env.insert(
        "OCM_INTERNAL_MANAGED_NODE_ARCHIVE_SHA256".to_string(),
        archive_sha256,
    );
    server
}

#[derive(Clone, Copy)]
enum ManagedNodeArchiveKind {
    TarGz,
    Zip,
}

fn managed_node_archive_layout() -> (String, &'static str, &'static str, ManagedNodeArchiveKind) {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => panic!("unsupported test architecture for fake managed node archive: {other}"),
    };

    match std::env::consts::OS {
        "macos" => (
            format!("darwin-{arch}"),
            "bin/node",
            "lib/node_modules/npm/bin/npm-cli.js",
            ManagedNodeArchiveKind::TarGz,
        ),
        "linux" => (
            format!("linux-{arch}"),
            "bin/node",
            "lib/node_modules/npm/bin/npm-cli.js",
            ManagedNodeArchiveKind::TarGz,
        ),
        "windows" => (
            format!("win-{arch}"),
            "node.exe",
            "node_modules/npm/bin/npm-cli.js",
            ManagedNodeArchiveKind::Zip,
        ),
        other => panic!("unsupported test OS for fake managed node archive: {other}"),
    }
}

pub fn sha512_integrity(body: &[u8]) -> String {
    let digest = Sha512::digest(body);
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

pub fn run_ocm(cwd: &Path, env: &BTreeMap<String, String>, args: &[&str]) -> Output {
    run_ocm_binary(Path::new(env!("CARGO_BIN_EXE_ocm")), cwd, env, args)
}

pub fn run_ocm_binary(
    binary: &Path,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    args: &[&str],
) -> Output {
    let mut command = Command::new(binary);
    command.current_dir(cwd);
    command.args(args);
    command.env_clear();
    command.envs(env);
    command.output().unwrap()
}

pub fn run_ocm_with_stdin(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    args: &[&str],
    input: &str,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    command.current_dir(cwd);
    command.args(args);
    command.env_clear();
    command.envs(env);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn().unwrap();
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}
