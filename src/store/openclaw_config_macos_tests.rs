use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::{replace_private_config, write_private_config};

struct Reader {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Reader {
    fn spawn(path: &Path, uid: libc::uid_t, gid: libc::gid_t) -> Self {
        let mut command = Command::new("/usr/bin/python3");
        command
            .current_dir("/private/tmp")
            .args([
                "-I",
                "-S",
                "-B",
                "-c",
                r#"
import hashlib, os, signal, sys
signal.alarm(10)
try:
    held = os.open(sys.argv[1], os.O_RDONLY)
except PermissionError:
    held = None
print(str(os.geteuid()) + ':' + ('opened' if held is not None else 'denied'), flush=True)
if not sys.stdin.readline():
    raise SystemExit(2)
if held is not None:
    print('sha256:' + hashlib.sha256(os.read(held, 65536)).hexdigest(), flush=True)
    os.close(held)
else:
    try:
        later = os.open(sys.argv[1], os.O_RDONLY)
    except PermissionError:
        print('denied', flush=True)
    else:
        os.close(later)
        print('opened', flush=True)
"#,
            ])
            .arg(path)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // The privileged test parent retains custody; only this reader drops
        // groups and identity before executing its fixed, read-only program.
        unsafe {
            command.pre_exec(move || {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "reader ended before its observation"
        );
        line.trim_end().to_string()
    }

    fn finish(&mut self) -> String {
        self.input.write_all(b"read\n").unwrap();
        let result = self.line();
        assert!(self.child.wait().unwrap().success());
        result
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn account_id(flag: &str) -> u32 {
    let output = Command::new("/usr/bin/id")
        .args([flag, "nobody"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "requires a privileged parent only to launch a distinct unprivileged reader; run by macOS CI"]
fn private_config_creation_excludes_other_users() {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run only this test through sudo on an isolated test host"
    );
    let uid = account_id("-u");
    let gid = account_id("-g");
    assert_ne!(uid, unsafe { libc::geteuid() });
    let root = tempfile::Builder::new()
        .prefix("ocm-private-config-access-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
    let acl = Command::new("/bin/chmod")
        .args([
            "+a",
            "everyone allow read,readattr,readextattr,readsecurity,file_inherit",
        ])
        .arg(root.path())
        .status()
        .unwrap();
    assert!(acl.success());
    let value =
        json!({"gateway": {"auth": {"mode": "token", "token": "synthetic-private-config"}}});
    let mut bytes = serde_json::to_vec_pretty(&value).unwrap();
    bytes.push(b'\n');
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let digest = format!("sha256:{digest}");

    // Reproduce the previous create -> clear ACL -> write boundary. The reader
    // opens while the inode is empty, so no timing race or secret transfer is
    // needed to prove that its descriptor retains access to future bytes.
    let mut legacy = tempfile::Builder::new()
        .prefix(".openclaw-config-")
        .tempfile_in(root.path())
        .unwrap();
    let mut old_reader = Reader::spawn(legacy.path(), uid, gid);
    assert_eq!(old_reader.line(), format!("{uid}:opened"));
    assert!(
        Command::new("/bin/chmod")
            .arg("-N")
            .arg(legacy.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(crate::infra::macos_security::file_has_no_extended_acl(legacy.as_file()).unwrap());
    write_private_config(&mut legacy, &value).unwrap();
    assert_eq!(old_reader.finish(), digest);
    drop(old_reader);
    drop(legacy);

    // Use the production creation and serialization boundaries, with the
    // unauthorized open placed before any configuration bytes are written.
    let mut staged = tempfile::Builder::new()
        .prefix(".openclaw-config-")
        .make_in(
            root.path(),
            crate::infra::macos_security::create_private_file_new,
        )
        .unwrap();
    let flags = unsafe { libc::fcntl(staged.as_file().as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    let mut reader = Reader::spawn(staged.path(), uid, gid);
    assert_eq!(reader.line(), format!("{uid}:denied"));
    write_private_config(&mut staged, &value).unwrap();
    assert_eq!(reader.finish(), "denied");
    drop(reader);
    let config = root.path().join("openclaw.json");
    replace_private_config(staged, &config).unwrap();
    assert!(std::fs::read(&config).unwrap() == bytes);
    root.close().unwrap();
    println!(
        "MAC_PRIVATE_CONFIG legacy_retained_read=true atomic_open_denied_before_and_after_write=true owner_read=true cleanup=true"
    );
}
