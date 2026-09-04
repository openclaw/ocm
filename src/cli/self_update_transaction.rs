//! Single-executable update journal. The retained, initiating OCM binary owns
//! both forward progress and recovery; candidate code never owns rollback.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::Cli;
use super::self_cmd::{StagedBinary, validate_staged_binary};
use crate::infra::download::file_sha256;
use crate::service::wait_for_gateway_readiness;
use crate::store::{resolve_ocm_home, resolve_user_home};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Phase {
    Prepared,
    Applying,
    RollingBack,
    Updated,
    RolledBack,
    RollbackFailed,
    Failed,
}

impl Phase {
    fn terminal(self) -> bool {
        matches!(self, Self::Updated | Self::RolledBack | Self::Failed)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Receipt {
    schema: u32,
    pub id: String,
    pub phase: Phase,
    pub binary_path: PathBuf,
    ocm_home: PathBuf,
    user_home: PathBuf,
    pub current_version: String,
    pub target_version: String,
    previous_sha256: String,
    candidate_sha256: String,
    pub daemon_was_running: bool,
    pub gateways: Vec<String>,
    pub error: Option<String>,
    pub rollback_error: Option<String>,
}

pub(super) struct Transaction {
    root: PathBuf,
    // Dropping the descriptor, rather than explicitly unlocking it, also works
    // if a process dies. The helper opens its own descriptor after admission.
    _lock: File,
}

fn transaction_root(binary: &Path) -> Result<PathBuf, String> {
    let name = binary.file_name().ok_or("executable has no filename")?;
    let mut directory = std::ffi::OsString::from(".");
    directory.push(name);
    directory.push(".self-update");
    Ok(binary
        .parent()
        .ok_or("executable has no parent")?
        .join(directory))
}

pub(super) fn receipt_path(binary: &Path) -> Result<PathBuf, String> {
    Ok(transaction_root(binary)?.join("receipt.json"))
}

fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("failed to sync {}: {error}", path.display()))?;
    Ok(())
}

impl Transaction {
    pub(super) fn open(binary: &Path, wait: bool) -> Result<Self, String> {
        let root = transaction_root(binary)?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&root) {
            Ok(()) => sync_dir(root.parent().unwrap())?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("cannot create update journal: {error}")),
        }
        let metadata = fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("self-update journal must be a private directory, not a symlink".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            // SAFETY: geteuid has no preconditions.
            if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
                return Err("self-update journal must be owned by this user with mode 0700".into());
            }
            let mut options = OpenOptions::new();
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
            return Self::lock(root, options, wait);
        }
        #[cfg(not(unix))]
        Self::lock(root, OpenOptions::new(), wait)
    }

    fn lock(root: PathBuf, mut options: OpenOptions, wait: bool) -> Result<Self, String> {
        let file = options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("lock"))
            .map_err(|e| e.to_string())?;
        if wait {
            FileExt::lock_exclusive(&file)
        } else {
            FileExt::try_lock_exclusive(&file)
        }
        .map_err(|error| {
            format!("self-update is busy; inspect `ocm self update --status`: {error}")
        })?;
        Ok(Self { root, _lock: file })
    }

    fn read(&self) -> Result<Receipt, String> {
        read_receipt(&self.root)
    }

    #[cfg(unix)]
    fn retain_lock_in_subcommands(&self) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        // After a worker crash, admission must remain locked while an already
        // started service-manager command can still change the daemon.
        let fd = self._lock.as_raw_fd();
        // SAFETY: fd is owned by self and remains open throughout both calls.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        Ok(())
    }

    pub(super) fn admit(&self) -> Result<(), String> {
        if self.root.join("receipt.json").exists() && !self.read()?.phase.terminal() {
            return Err(format!(
                "unfinished self-update at {}; inspect --status, then use --recover",
                self.root.display()
            ));
        }
        Ok(())
    }

    fn save(&self, receipt: &Receipt) -> Result<(), String> {
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(&mut file, receipt).map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(self.root.join("receipt.json"))
            .map_err(|e| e.to_string())?;
        sync_dir(&self.root)
    }

    pub(super) fn launch(
        self,
        cli: &Cli,
        binary: &Path,
        candidate: StagedBinary,
        current_version: &str,
        target_version: &str,
    ) -> Result<Receipt, String> {
        self.admit()?;
        // All fallible staging happens before publishing the nonterminal journal.
        // Keep the previous executable intact even when it is the running helper.
        let previous = StagedBinary::copy_from(binary, &self.root)?;
        validate_staged_binary(previous.path(), current_version)?;
        let receipt = Receipt {
            schema: 1,
            id: format!(
                "{}-{}",
                std::process::id(),
                time::OffsetDateTime::now_utc().unix_timestamp_nanos()
            ),
            phase: Phase::Prepared,
            binary_path: binary.to_path_buf(),
            ocm_home: resolve_ocm_home(&cli.env, &cli.cwd)?,
            user_home: resolve_user_home(&cli.env),
            current_version: current_version.into(),
            target_version: target_version.into(),
            previous_sha256: file_sha256(previous.path())?,
            candidate_sha256: file_sha256(candidate.path())?,
            daemon_was_running: false,
            gateways: Vec::new(),
            error: None,
            rollback_error: None,
        };
        fs::rename(previous.path(), self.root.join("previous")).map_err(|e| e.to_string())?;
        fs::rename(candidate.path(), self.root.join("candidate")).map_err(|e| e.to_string())?;
        sync_dir(&self.root)?;
        self.save(&receipt)?;
        self.start_and_wait(cli, receipt)
    }

    fn start_and_wait(self, cli: &Cli, receipt: Receipt) -> Result<Receipt, String> {
        let mut child = spawn_worker(cli, &self.root.join("previous"), &receipt)?;
        let root = self.root.clone();
        let id = receipt.id.clone();
        cli.stderr_line(format!(
            "Self-update {id}; reconnect with `ocm self update --status`. Recovery executable: {}",
            root.join("previous").display()
        ));
        // Prepared state prevents another updater from entering this handoff gap.
        drop(self);
        let status = child.wait().map_err(|e| e.to_string())?;
        let result = read_receipt(&root)?;
        if result.id != id {
            return Err("self-update receipt changed while waiting".into());
        }
        if !status.success() || !result.phase.terminal() {
            return Err(format!(
                "self-update did not finish; inspect {} and run --recover. {:?}: {}",
                root.join("receipt.json").display(),
                result.phase,
                result
                    .rollback_error
                    .as_deref()
                    .or(result.error.as_deref())
                    .unwrap_or("helper exited")
            ));
        }
        Ok(result)
    }
}

fn read_receipt(root: &Path) -> Result<Receipt, String> {
    let data = fs::read(root.join("receipt.json"))
        .map_err(|error| format!("cannot read self-update receipt: {error}"))?;
    let receipt: Receipt =
        serde_json::from_slice(&data).map_err(|e| format!("invalid self-update receipt: {e}"))?;
    if receipt.schema != 1 || transaction_root(&receipt.binary_path)? != root {
        return Err("unsupported or misplaced self-update receipt".into());
    }
    Ok(receipt)
}

fn spawn_worker(cli: &Cli, previous: &Path, receipt: &Receipt) -> Result<Child, String> {
    #[cfg(not(unix))]
    {
        let _ = (cli, previous, receipt);
        Err("recoverable self-update is unsupported on this platform".into())
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut command = worker_command(previous, receipt)?;
        command
            .env_clear()
            .envs(&cli.env)
            .env_remove("OCM_ACTIVE_ENV")
            .env_remove("OPENCLAW_SERVICE_KIND")
            .env("OCM_SERVICE_EXECUTABLE", &receipt.binary_path)
            .current_dir(&cli.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: only async-signal-safe syscalls in the post-fork child. A new
        // session also releases the controlling terminal and source process group.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
        command.spawn().map_err(|e| {
            format!("cannot detach self-update helper; no executable was replaced: {e}")
        })
    }
}

#[cfg(unix)]
fn worker_command(previous: &Path, receipt: &Receipt) -> Result<Command, String> {
    #[cfg(target_os = "linux")]
    let in_service = fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| format!("cannot inspect helper containment: {e}"))?
        .split('/')
        .any(|component| component.trim_end() == "ai.openclaw.ocm.service");
    #[cfg(not(target_os = "linux"))]
    let in_service = false;

    let mut command = if in_service {
        // setsid cannot escape systemd KillMode=control-group. A transient user
        // scope moves the helper out before exec, preserving its environment.
        let mut scope = Command::new("systemd-run");
        scope
            .args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit=ocm-self-update-{}", receipt.id))
            .arg("--")
            .arg(previous);
        scope
    } else {
        Command::new(previous)
    };
    command
        .arg("__daemon")
        .arg("self-update")
        .arg(&receipt.binary_path)
        .arg(&receipt.id);
    Ok(command)
}

impl Cli {
    pub(super) fn self_update_receipt(&self, recover: bool) -> Result<Receipt, String> {
        let binary = std::env::current_exe().map_err(|e| e.to_string())?;
        // The retained helper remains usable even when the candidate cannot run.
        let root = if binary.file_name().is_some_and(|name| name == "previous") {
            binary
                .parent()
                .ok_or("recovery executable has no parent")?
                .to_path_buf()
        } else {
            transaction_root(&binary)?
        };
        let mut receipt = read_receipt(&root)?;
        self.validate_receipt_home(&receipt)?;
        if !recover || receipt.phase.terminal() {
            return Ok(receipt);
        }
        let transaction = Transaction::open(&receipt.binary_path, false)?;
        receipt = transaction.read()?;
        if receipt.phase.terminal() {
            return Ok(receipt);
        }
        receipt.phase = Phase::RollingBack;
        receipt
            .error
            .get_or_insert("recovery requested after interrupted self-update".into());
        transaction.save(&receipt)?;
        transaction.start_and_wait(self, receipt)
    }

    fn validate_receipt_home(&self, receipt: &Receipt) -> Result<(), String> {
        if resolve_ocm_home(&self.env, &self.cwd)? != receipt.ocm_home
            || resolve_user_home(&self.env) != receipt.user_home
        {
            return Err(
                "self-update belongs to a different HOME or OCM_HOME; use the original store"
                    .into(),
            );
        }
        Ok(())
    }

    pub(super) fn run_self_update_worker(&self, args: Vec<String>) -> Result<i32, String> {
        let [binary, id] = args.as_slice() else {
            return Err("internal self-update requires binary and transaction id".into());
        };
        let transaction = Transaction::open(Path::new(binary), true)?;
        #[cfg(unix)]
        transaction.retain_lock_in_subcommands()?;
        let mut receipt = transaction.read()?;
        self.validate_receipt_home(&receipt)?;
        if &receipt.id != id {
            return Err("stale self-update helper".into());
        }
        if receipt.phase.terminal() {
            return Ok(0);
        }
        let mut env = self.env.clone();
        env.remove("OCM_ACTIVE_ENV");
        env.remove("OPENCLAW_SERVICE_KIND");
        env.insert("OCM_SERVICE_EXECUTABLE".into(), binary.clone());
        let worker = Cli {
            env,
            cwd: receipt.ocm_home.clone(),
        };
        let result = worker.perform_update(&transaction, &mut receipt);
        if let Err(error) = result {
            receipt.error.get_or_insert(error);
            if receipt.phase == Phase::Prepared {
                receipt.phase = Phase::Failed;
            } else {
                receipt.phase = Phase::RollingBack;
                transaction.save(&receipt)?;
                match worker.rollback_update(&transaction, &receipt) {
                    Ok(()) => {
                        receipt.phase = Phase::RolledBack;
                        receipt.rollback_error = None;
                    }
                    Err(error) => {
                        receipt.phase = Phase::RollbackFailed;
                        receipt.rollback_error = Some(error);
                    }
                }
            }
        }
        transaction.save(&receipt)?;
        Ok(if receipt.phase == Phase::RollbackFailed {
            1
        } else {
            0
        })
    }

    fn perform_update(
        &self,
        transaction: &Transaction,
        receipt: &mut Receipt,
    ) -> Result<(), String> {
        // Keep lifecycle admission through convergence AND rollback. Acquiring
        // outside this function would recursively lock the ordinary refresh API.
        let service = self.supervisor_service();
        let _lifecycle = service.lock_daemon_lifecycle()?;
        service.validate_self_update_daemon_locked()?;
        if receipt.phase != Phase::Prepared {
            return Err("interrupted update requires rollback".into());
        }
        if file_sha256(&receipt.binary_path)? != receipt.previous_sha256 {
            return Err("installed executable changed after staging; refusing replacement".into());
        }
        let candidate = transaction.root.join("candidate");
        if file_sha256(&candidate)? != receipt.candidate_sha256 {
            return Err("staged candidate changed after verification".into());
        }
        validate_staged_binary(&candidate, &receipt.target_version)?;
        let before = service.daemon_status()?;
        receipt.daemon_was_running = before.running;
        if before.running {
            let runtime = service.runtime()?;
            if !runtime.present
                || runtime.daemon_version.as_deref() != Some(&receipt.current_version)
            {
                return Err("running daemon version is unknown or already skewed; refresh it before self-update".into());
            }
            receipt.gateways = runtime
                .children
                .into_iter()
                .map(|child| child.env_name)
                .collect();
        }
        receipt.phase = Phase::Applying;
        transaction.save(receipt)?;
        let result = (|| {
            fs::rename(&candidate, &receipt.binary_path)
                .map_err(|e| format!("cannot publish candidate: {e}"))?;
            sync_dir(receipt.binary_path.parent().unwrap())?;
            sync_dir(&transaction.root)?;
            validate_staged_binary(&receipt.binary_path, &receipt.target_version)?;
            if receipt.daemon_was_running {
                self.refresh_and_verify(receipt, &receipt.target_version)?;
            }
            receipt.phase = Phase::Updated;
            transaction.save(receipt)
        })();
        if let Err(error) = result {
            receipt.error = Some(error);
            receipt.phase = Phase::RollingBack;
            transaction.save(receipt)?;
            // Do not release lifecycle ownership between forward failure and undo.
            match self.rollback_update_locked(transaction, receipt) {
                Ok(()) => receipt.phase = Phase::RolledBack,
                Err(error) => {
                    receipt.phase = Phase::RollbackFailed;
                    receipt.rollback_error = Some(error);
                }
            }
        }
        Ok(())
    }

    fn rollback_update(&self, transaction: &Transaction, receipt: &Receipt) -> Result<(), String> {
        let service = self.supervisor_service();
        let _lifecycle = service.lock_daemon_lifecycle()?;
        service.validate_self_update_daemon_locked()?;
        self.rollback_update_locked(transaction, receipt)
    }

    fn rollback_update_locked(
        &self,
        transaction: &Transaction,
        receipt: &Receipt,
    ) -> Result<(), String> {
        let previous = transaction.root.join("previous");
        if file_sha256(&previous)? != receipt.previous_sha256 {
            return Err("retained previous executable changed; refusing rollback".into());
        }
        let installed = file_sha256(&receipt.binary_path)?;
        if installed != receipt.previous_sha256 && installed != receipt.candidate_sha256 {
            return Err("installed executable has unrelated bytes; refusing rollback".into());
        }
        let restore = StagedBinary::copy_from(&previous, &transaction.root)?;
        fs::rename(restore.path(), &receipt.binary_path).map_err(|e| e.to_string())?;
        sync_dir(receipt.binary_path.parent().unwrap())?;
        sync_dir(&transaction.root)?;
        validate_staged_binary(&receipt.binary_path, &receipt.current_version)?;
        if receipt.daemon_was_running {
            self.refresh_and_verify(receipt, &receipt.current_version)?;
        }
        Ok(())
    }

    fn refresh_and_verify(&self, receipt: &Receipt, version: &str) -> Result<(), String> {
        let service = self.supervisor_service();
        let old_pid = service.daemon_status()?.pid;
        let started = time::OffsetDateTime::now_utc();
        // Same activation primitive as explicit refresh, without rebuilding desired
        // state or moving this already-detached helper back into the caller group.
        service.activate_daemon("self-update")?;
        let timeout = self
            .env
            .get("OCM_INTERNAL_SELF_UPDATE_TIMEOUT_MS")
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(30));
        let deadline = Instant::now() + timeout;
        loop {
            let daemon = service.daemon_status()?;
            let runtime = service.runtime()?;
            if daemon.running
                && daemon.pid.is_some()
                && daemon.pid != old_pid
                && runtime.present
                && runtime.updated_at >= started
                && runtime.daemon_version.as_deref() == Some(version)
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!("daemon did not converge to OCM {version}"));
            }
            sleep(Duration::from_millis(100));
        }
        for name in &receipt.gateways {
            let ready = wait_for_gateway_readiness(name, &self.env, &self.cwd)?;
            if !ready.ready {
                return Err(format!(
                    "gateway {name} did not recover: {}",
                    ready.issue.unwrap_or_default()
                ));
            }
        }
        Ok(())
    }
}
