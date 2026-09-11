use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(any(not(windows), test))]
use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::EnvironmentService;
use super::source_watch_session::{
    DevUiChildRole, SourceUiTarget, SourceWatchSession, SourceWatchSessionPaths,
};
use crate::infra::process_identity::{
    ProcessIdentity, current_process_identity, observe_process, process_scope_id,
};
use crate::service::platform::{ServiceManagerKind, service_manager_kind};
use crate::store::{
    ExclusiveFileLock, display_path, ensure_dir, lock_file, now_utc, read_json,
    source_watch_override_path, try_lock_file, validate_name, write_json,
};
use crate::supervisor::SupervisorService;

const SOURCE_WATCH_OVERRIDE_KIND: &str = "ocm-source-watch-override";
const SOURCE_WATCH_LOCK_RETRY_ATTEMPTS: usize = 20;
const SOURCE_WATCH_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
#[cfg(windows)]
const SOURCE_WATCH_GENERATION_MAX_BYTES: usize = 1024;
#[cfg(windows)]
const SOURCE_WATCH_LOCK_BYTE_OFFSET: u32 = 4096;
#[cfg(windows)]
const _: () =
    assert!(SOURCE_WATCH_GENERATION_MAX_BYTES + 1 < SOURCE_WATCH_LOCK_BYTE_OFFSET as usize);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceWatchOverride {
    pub kind: String,
    pub env_name: String,
    pub repo_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<SourceWatchEndpoint>,
    pub watch_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watching: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<SourceWatchUiEndpoint>,
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceWatchUiEndpoint {
    pub port: u32,
    pub pid: u32,
    pub gateway_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceWatchEndpoint {
    pub env_root: String,
    pub gateway_port: u32,
}

#[derive(Clone, Debug)]
pub(crate) enum SourceWatchState {
    Inactive,
    Starting,
    Active(SourceWatchOverride),
    Restoring,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum SourceWatchMode {
    Foreground { watching: bool },
    ServicePreparation,
}

impl SourceWatchMode {
    pub(super) fn is_watching(self) -> bool {
        matches!(self, Self::Foreground { watching: true })
    }

    pub(super) fn is_service_preparation(self) -> bool {
        matches!(self, Self::ServicePreparation)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CreateSourceWatchOverrideOptions {
    pub(crate) env_name: String,
    pub(crate) repo_root: PathBuf,
    pub(crate) endpoint: SourceWatchEndpoint,
    pub(crate) watch_pid: u32,
}

#[derive(Debug)]
pub(crate) struct SourceWatchLease {
    env_name: String,
    lease_id: String,
    lock_file: File,
    service_was_running: bool,
    service_preparation_revision: Option<u64>,
    watching: bool,
    session_paths: SourceWatchSessionPaths,
    session: Option<SourceWatchSession>,
    #[cfg(windows)]
    lease_event: WindowsSourceWatchEvent,
}

pub(crate) struct SourceWatchLeaseObservation {
    pub(crate) lease_id: String,
    pub(crate) held: bool,
}

impl SourceWatchLease {
    pub(crate) fn is_watching(&self) -> bool {
        self.watching
    }

    pub(crate) fn has_ui(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.ui.is_some())
    }

    pub(crate) fn service_was_running(&self) -> bool {
        self.service_was_running
    }

    pub(crate) fn service_preparation_revision(&self) -> Option<u64> {
        self.service_preparation_revision
    }

    pub(crate) fn session(&self) -> Option<&SourceWatchSession> {
        self.session.as_ref()
    }

    pub(crate) fn lease_id(&self) -> &str {
        &self.lease_id
    }

    pub(crate) fn stop_requested(&self) -> Result<bool, String> {
        self.session
            .as_ref()
            .map(|session| self.session_paths.stop_requested(session))
            .unwrap_or(Ok(false))
    }

    pub(crate) fn begin_child_spawn(&mut self) -> Result<(), String> {
        if self.has_ui() {
            return self.begin_ui_child_spawn(DevUiChildRole::Command);
        }
        if let Some(session) = &mut self.session {
            session.child = None;
            session.child_spawn_pending = true;
            self.session_paths.save_session(session)?;
        }
        Ok(())
    }

    pub(crate) fn record_child(&mut self, pid: u32) -> Result<(), String> {
        if self.has_ui() {
            return self
                .record_ui_child(DevUiChildRole::Command, pid)
                .map(|_| ());
        }
        if let Some(session) = &mut self.session {
            let process = observe_process(pid)?.ok_or_else(|| {
                format!("failed to inspect source watch child identity for pid {pid}")
            })?;
            session.child = Some(process.identity);
            session.child_spawn_pending = false;
            self.session_paths.save_session(session)?;
        }
        Ok(())
    }

    pub(crate) fn clear_child(&mut self) -> Result<(), String> {
        if self.has_ui() {
            let expected = self
                .session
                .as_ref()
                .and_then(|session| session.ui.as_ref())
                .and_then(|ui| ui.children.get(DevUiChildRole::Command))
                .cloned();
            return self.clear_ui_child(DevUiChildRole::Command, expected.as_ref());
        }
        if let Some(session) = &mut self.session {
            session.child = None;
            session.child_spawn_pending = false;
            self.session_paths.save_session(session)?;
        }
        Ok(())
    }

    pub(crate) fn claim_ui_target(&mut self, target: SourceUiTarget) -> Result<(), String> {
        if !target.valid() {
            return Err("the dev UI target is invalid".to_string());
        }
        let session = self.session.as_mut().ok_or("dev UI session is missing")?;
        if session.closed || session.has_child_ownership() {
            return Err("dev UI target cannot change while a child is owned".to_string());
        }
        let ui = session
            .ui
            .as_mut()
            .ok_or("dev session did not request a UI")?;
        if ui.target.as_ref().is_some_and(|current| current != &target) {
            return Err("dev UI target was already captured".to_string());
        }
        ui.target = Some(target);
        self.session_paths.save_session(session)
    }

    pub(crate) fn begin_ui_child_spawn(&mut self, role: DevUiChildRole) -> Result<(), String> {
        let session = self.session.as_mut().ok_or("dev UI session is missing")?;
        let ui = session
            .ui
            .as_mut()
            .ok_or("dev session did not request a UI")?;
        if session.closed
            || ui.pending.is_some()
            || ui.children.get(role).is_some()
            || (role != DevUiChildRole::Command && ui.target.is_none())
        {
            return Err("dev UI child cannot replace existing ownership".to_string());
        }
        ui.pending = Some(role);
        self.session_paths.save_session(session)
    }

    pub(crate) fn record_ui_child(
        &mut self,
        role: DevUiChildRole,
        pid: u32,
    ) -> Result<ProcessIdentity, String> {
        let process =
            observe_process(pid)?.ok_or_else(|| format!("failed to inspect dev UI child {pid}"))?;
        let session = self.session.as_mut().ok_or("dev UI session is missing")?;
        if session
            .recorded_children()
            .iter()
            .any(|(_, child)| child.pid == pid)
            || session.controller.pid == pid
        {
            return Err("dev UI child identity is already owned".to_string());
        }
        let ui = session
            .ui
            .as_mut()
            .ok_or("dev session did not request a UI")?;
        if ui.pending != Some(role) || ui.children.get(role).is_some() {
            return Err("dev UI child has no matching pending spawn".to_string());
        }
        *ui.children.get_mut(role) = Some(process.identity.clone());
        ui.pending = None;
        self.session_paths.save_session(session)?;
        Ok(process.identity)
    }

    pub(crate) fn clear_ui_child(
        &mut self,
        role: DevUiChildRole,
        expected: Option<&ProcessIdentity>,
    ) -> Result<(), String> {
        let session = self.session.as_mut().ok_or("dev UI session is missing")?;
        let ui = session
            .ui
            .as_mut()
            .ok_or("dev session did not request a UI")?;
        if ui
            .children
            .get(role)
            .is_some_and(|child| Some(child) != expected)
            || (expected.is_some() && ui.pending == Some(role))
        {
            return Err("dev UI child changed; refusing stale cleanup".to_string());
        }
        *ui.children.get_mut(role) = None;
        if ui.pending == Some(role) {
            ui.pending = None;
        }
        self.session_paths.save_session(session)
    }

    pub(crate) fn begin_service_takeover(&mut self) -> Result<(), String> {
        if let Some(session) = &mut self.session {
            session.restore_service = self.service_was_running;
            self.session_paths.save_session(session)?;
        }
        Ok(())
    }

    pub(crate) fn discard_service_restore(&mut self) -> Result<(), String> {
        if let Some(session) = &mut self.session {
            session.restore_service = false;
            self.session_paths.save_session(session)?;
        }
        Ok(())
    }

    pub(crate) fn finish_session(
        &mut self,
        service_restored: bool,
        error: Option<String>,
        preserve_session: bool,
    ) -> Result<(), String> {
        let _admission = lock_file(&self.session_paths.admission, "gateway admission")?;
        if let Some(session) = &self.session {
            self.session_paths
                .finish(session, service_restored, error, preserve_session)?;
            if !preserve_session {
                self.session = None;
            }
        }
        Ok(())
    }

    pub(crate) fn acknowledge_stopped_processes(&mut self) -> Result<(), String> {
        let session = self
            .session
            .as_mut()
            .ok_or("source watch session is missing")?;
        session.child = None;
        session.child_spawn_pending = false;
        if let Some(ui) = &mut session.ui {
            ui.children = Default::default();
            ui.pending = None;
        }
        session.restore_service = false;
        // Publish closure only after request/endpoint cleanup succeeds. Until
        // then the persisted failure and child identities remain available for
        // another recovery attempt. The operator owns detached-process proof.
        self.finish_session(false, None, false)
    }

    pub(crate) fn begin_service_restore(&mut self) -> Result<(), String> {
        let _admission = lock_file(&self.session_paths.admission, "gateway admission")?;
        write_source_watch_lock(&mut self.lock_file, &format!("restoring:{}", self.lease_id))
    }

    #[cfg(unix)]
    pub(crate) fn configure_child(&self, command: &mut Command) {
        let lock_fd = self.lock_file.as_raw_fd();
        command.process_group(0);
        // The watcher inherits the lease so an OCM crash cannot release
        // exclusivity while the source gateway remains alive.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(lock_fd, libc::F_GETFD);
                if flags == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fcntl(lock_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn configure_child(&self, _command: &mut Command) {}

    #[cfg(windows)]
    pub(crate) fn attach_to_child(&self, child: &std::process::Child) -> Result<(), String> {
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let mut child_handle: HANDLE = std::ptr::null_mut();
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.lease_event.handle,
                child.as_raw_handle() as HANDLE,
                &mut child_handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(format!(
                "failed attaching source watch lease to child: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    pub(crate) fn attach_to_child(&self, _child: &std::process::Child) -> Result<(), String> {
        Ok(())
    }
}

impl SourceWatchOverride {
    pub fn openclaw_entry_path(&self) -> PathBuf {
        Path::new(&self.repo_root).join("openclaw.mjs")
    }

    pub fn extensions_dir(&self) -> PathBuf {
        Path::new(&self.repo_root).join("extensions")
    }

    pub fn command_label(&self) -> String {
        format!("node {}", display_path(&self.openclaw_entry_path()))
    }
}

impl<'a> EnvironmentService<'a> {
    pub(crate) fn observe_source_watch_lease(
        &self,
        env_name: &str,
    ) -> Result<Option<SourceWatchLeaseObservation>, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        let lock_path =
            source_watch_override_path(&env_name, self.env, self.cwd)?.with_extension("lock");
        let mut file = match OpenOptions::new().read(true).open(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("failed reading source watch lease: {error}")),
        };
        let held = match try_lock_source_watch_file(&file, false) {
            Ok(()) => false,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
            Err(error) => return Err(format!("failed inspecting source watch lease: {error}")),
        };
        #[cfg(windows)]
        let held = held || open_windows_source_watch_event(&lock_path)?.is_some();
        let value = read_source_watch_lock(&mut file)
            .map_err(|error| format!("failed reading source watch lease: {error}"))?;
        let value = value.trim();
        Ok(Some(SourceWatchLeaseObservation {
            lease_id: value
                .strip_prefix("restoring:")
                .unwrap_or(value)
                .to_string(),
            held,
        }))
    }

    // The caller holds the environment operation and admission locks, and has
    // verified that the recorded controller and its owned process group stopped.
    pub(crate) fn reclaim_source_watch_lease_locked(
        &self,
        session: SourceWatchSession,
    ) -> Result<SourceWatchLease, String> {
        let mut lease = self.lock_source_watch_recovery_locked(session)?;
        let session = lease
            .session
            .as_mut()
            .ok_or("source watch session is missing")?;
        session.controller = current_process_identity()?;
        session.process_scope = process_scope_id()?;
        session.child = None;
        session.child_spawn_pending = false;
        if let Some(ui) = &mut session.ui {
            ui.children = Default::default();
            ui.pending = None;
        }
        session.closed = false;
        session.completion = None;
        lease.session_paths.save_session(session)?;
        Ok(lease)
    }

    // Caller holds operation/admission. Acquiring this exact-generation lease
    // does not change the persisted session or claim that cleanup succeeded.
    pub(crate) fn lock_source_watch_recovery_locked(
        &self,
        session: SourceWatchSession,
    ) -> Result<SourceWatchLease, String> {
        let override_path = source_watch_override_path(&session.env_name, self.env, self.cwd)?;
        let lock_path = override_path.with_extension("lock");
        let mut lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| format!("failed opening source watch recovery lease: {error}"))?;
        #[cfg(windows)]
        let lease_event = WindowsSourceWatchEvent {
            handle: acquire_windows_source_watch_event(&lock_path, &session.env_name)?,
        };
        try_lock_source_watch_exclusive(&lock_file).map_err(|error| {
            format!("source watch lease is still held; recovery was not completed: {error}")
        })?;
        let value = read_source_watch_lock(&mut lock_file)
            .map_err(|error| format!("failed reading source watch recovery lease: {error}"))?;
        if value
            .trim()
            .strip_prefix("restoring:")
            .unwrap_or(value.trim())
            != session.lease_id
        {
            return Err("source watch generation changed; refusing stale recovery".to_string());
        }
        let session_paths = SourceWatchSessionPaths::from_override(&override_path);
        Ok(SourceWatchLease {
            env_name: session.env_name.clone(),
            lease_id: session.lease_id.clone(),
            lock_file,
            service_was_running: session.restore_service,
            service_preparation_revision: None,
            watching: session.is_watching(),
            session_paths,
            session: Some(session),
            #[cfg(windows)]
            lease_event,
        })
    }

    pub(crate) fn clear_source_watch_override_for_lease(
        &self,
        env_name: &str,
        lease_id: &str,
    ) -> Result<(), String> {
        let env_name = validate_name(env_name, "Environment name")?;
        let path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        if !path.try_exists().map_err(|error| {
            format!(
                "failed inspecting source watch override {}: {error}",
                display_path(&path)
            )
        })? {
            return Ok(());
        }
        let meta = read_json::<SourceWatchOverride>(&path)?;
        if !source_watch_matches_lease(&meta, lease_id) {
            return Err("source watch override changed; refusing stale cleanup".to_string());
        }
        self.clear_source_watch_override(&env_name, &meta.token)?;
        Ok(())
    }

    pub(crate) fn lock_gateway_admission(
        &self,
        env_name: &str,
    ) -> Result<ExclusiveFileLock, String> {
        lock_file(&self.gateway_admission_path(env_name)?, "gateway admission")
    }

    pub(crate) fn try_lock_gateway_admission(
        &self,
        env_name: &str,
    ) -> Result<Option<ExclusiveFileLock>, String> {
        try_lock_file(&self.gateway_admission_path(env_name)?, "gateway admission")
    }

    fn gateway_admission_path(&self, env_name: &str) -> Result<PathBuf, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        Ok(source_watch_override_path(&env_name, self.env, self.cwd)?.with_extension("admission"))
    }

    pub(crate) fn ensure_source_watch_allows_state_mutation_locked(
        &self,
        name: &str,
    ) -> Result<(), String> {
        let unverified = |error: String| {
            format!(
                "cannot verify dev ownership for env {name}: {error}; verified operator recovery is required before changing env state; preserve the environment and verify the watch processes and service policy before retrying"
            )
        };
        let session = self.source_watch_session(name).map_err(&unverified)?;
        let unfinished = session.is_some_and(|session| !session.closed);
        let state = match self.observe_source_watch(name).map_err(&unverified)? {
            SourceWatchState::Inactive => None,
            SourceWatchState::Starting => Some("starting"),
            SourceWatchState::Active(_) => Some("active"),
            SourceWatchState::Restoring => Some("restoring"),
        };
        if let Some(state) = state {
            let recovery = if unfinished {
                format!(
                    "request shutdown with ocm dev stop {name}; if ownership cannot be verified, verified operator recovery is required"
                )
            } else {
                "stop it from its original dev terminal; if that is unavailable, verified operator recovery is required".to_string()
            };
            return Err(format!(
                "cannot change env {name} while its dev session is {state}; {recovery}"
            ));
        }
        if unfinished {
            return Err(format!(
                "cannot change env {name} while its dev session is unfinished; request shutdown with ocm dev stop {name}; if ownership cannot be verified, verified operator recovery is required"
            ));
        }
        Ok(())
    }

    pub(crate) fn ensure_source_watch_allows_service(&self, env_name: &str) -> Result<(), String> {
        if let Some(session) = self.source_watch_session(env_name)? {
            if let Some(error) = session.unsafe_cleanup_error() {
                return Err(error.to_string());
            }
            if !session.closed
                && session.has_child_ownership()
                && !session.controller_is_running()?
            {
                return Err(format!(
                    "source watch ownership for env {env_name} is unfinished; run `ocm dev stop {env_name}` before starting its service"
                ));
            }
        }
        if self.active_source_watch_override(env_name)?.is_some() {
            return Err(format!(
                "background service for env \"{env_name}\" cannot start while source watch is active; stop the watch session first"
            ));
        }
        Ok(())
    }

    pub(crate) fn acquire_source_watch_lease(
        &self,
        env_name: &str,
        allow_service_takeover: bool,
        mode: SourceWatchMode,
    ) -> Result<SourceWatchLease, String> {
        self.acquire_foreground_lease(env_name, allow_service_takeover, mode, false)
    }

    pub(crate) fn acquire_source_ui_lease(
        &self,
        env_name: &str,
        allow_service_takeover: bool,
        watching: bool,
    ) -> Result<SourceWatchLease, String> {
        self.acquire_foreground_lease(
            env_name,
            allow_service_takeover,
            SourceWatchMode::Foreground { watching },
            true,
        )
    }

    fn acquire_foreground_lease(
        &self,
        env_name: &str,
        allow_service_takeover: bool,
        mode: SourceWatchMode,
        ui: bool,
    ) -> Result<SourceWatchLease, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        // Match service updates: operation, daemon lifecycle, then Gateway
        // admission. Keep the daemon generation stable through lease publication.
        let _operation_lock = self.lock_operation(&env_name)?;
        let supervisor = SupervisorService::new(self.env, self.cwd);
        let _lifecycle_lock = if service_manager_kind(self.env) == ServiceManagerKind::Unsupported {
            None
        } else {
            Some(supervisor.lock_daemon_lifecycle()?)
        };
        // A planner must not publish a pre-admission view after preparation
        // claims the environment. Take its existing state lock before admission.
        let _state_lock = mode
            .is_service_preparation()
            .then(|| supervisor.lock_state_publication())
            .transpose()?;
        let _admission_lock = self.lock_gateway_admission(&env_name)?;
        supervisor.ensure_source_watch_daemon_compatible()?;
        let meta = self.get(&env_name)?;
        let service_preparation_revision = mode
            .is_service_preparation()
            .then(|| {
                crate::store::environment_service_policy_revision(&env_name, self.env, self.cwd)
            })
            .transpose()?;
        if meta.service_running && !allow_service_takeover && !mode.is_service_preparation() {
            return Err(format!(
                "dev env {env_name} is already running in the background; stop it first or rerun with --watch --force to take it over temporarily"
            ));
        }
        let override_path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        let session_paths = SourceWatchSessionPaths::from_override(&override_path);
        let lock_path = override_path.with_extension("lock");
        if let Ok(meta) = read_json::<SourceWatchOverride>(&override_path)
            && !is_leased_source_watch(&meta)
            && is_valid_source_watch_metadata(&meta, &env_name)
            && is_legacy_source_watch_process(&meta)
        {
            return Err(format!(
                "source watch for env \"{env_name}\" is already active with legacy pid {}",
                meta.watch_pid
            ));
        }
        if let Some(parent) = lock_path.parent() {
            ensure_dir(parent)?;
        }
        let mut lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                format!(
                    "failed opening source watch lock {}: {error}",
                    display_path(&lock_path)
                )
            })?;
        #[cfg(windows)]
        let lease_event = WindowsSourceWatchEvent {
            handle: acquire_windows_source_watch_event(&lock_path, &env_name)?,
        };
        match try_lock_source_watch_exclusive(&lock_file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(format!(
                    "source watch for env \"{env_name}\" is already active or starting"
                ));
            }
            Err(error) => {
                return Err(format!(
                    "failed locking source watch for env \"{env_name}\": {error}"
                ));
            }
        }

        session_paths.ensure_previous_session_finished(&env_name)?;

        let lease_id = format!(
            "{}-{}",
            std::process::id(),
            now_utc().unix_timestamp_nanos()
        );
        // Publish the new generation before touching stale metadata. Readers that
        // observe this exclusive owner must never correlate an old token with it.
        write_source_watch_lock(&mut lock_file, &lease_id)?;

        // Owning the OS lock proves any surviving metadata belongs to a dead
        // lease, even if its child PID has since been reused by another process.
        remove_file_if_present(&override_path)?;
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        let session = Some(if ui {
            session_paths.create_ui_session(&meta, &lease_id, mode.is_watching())?
        } else {
            session_paths.create_session(&meta, &lease_id, mode)?
        });
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        let session = None;
        Ok(SourceWatchLease {
            env_name,
            lease_id,
            lock_file,
            service_was_running: meta.service_running && !mode.is_service_preparation(),
            service_preparation_revision,
            watching: mode.is_watching(),
            session_paths,
            session,
            #[cfg(windows)]
            lease_event,
        })
    }

    pub(crate) fn create_source_watch_override_with_lease(
        &self,
        options: CreateSourceWatchOverrideOptions,
        lease: &SourceWatchLease,
    ) -> Result<SourceWatchOverride, String> {
        if options.env_name != lease.env_name {
            return Err(format!(
                "source watch lease for env \"{}\" cannot create an override for env \"{}\"",
                lease.env_name, options.env_name
            ));
        }
        let ui = if let Some(ui) = lease.session().and_then(|session| session.ui.as_ref()) {
            let target = ui.target.as_ref().ok_or("dev UI target was not captured")?;
            if ui
                .children
                .get(DevUiChildRole::Gateway)
                .map(|child| child.pid)
                != Some(options.watch_pid)
            {
                return Err("dev Gateway does not match its recorded UI session".to_string());
            }
            Some(SourceWatchUiEndpoint {
                port: target.port,
                pid: ui
                    .children
                    .get(DevUiChildRole::Ui)
                    .ok_or("dev UI child was not recorded")?
                    .pid,
                gateway_url: target.gateway_url.clone(),
            })
        } else {
            None
        };
        self.write_source_watch_override(options, &lease.lease_id, lease.watching, ui)
    }

    fn write_source_watch_override(
        &self,
        options: CreateSourceWatchOverrideOptions,
        lease_id: &str,
        watching: bool,
        ui: Option<SourceWatchUiEndpoint>,
    ) -> Result<SourceWatchOverride, String> {
        let env_name = validate_name(&options.env_name, "Environment name")?;
        let path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        let token = format!(
            "lease:{lease_id}:{}-{}",
            options.watch_pid,
            now_utc().unix_timestamp_nanos()
        );
        let meta = SourceWatchOverride {
            kind: SOURCE_WATCH_OVERRIDE_KIND.to_string(),
            env_name,
            repo_root: display_path(&options.repo_root),
            endpoint: Some(options.endpoint),
            watch_pid: options.watch_pid,
            watching: Some(watching),
            ui,
            token,
            started_at: now_utc(),
        };
        write_json(&path, &meta)?;
        Ok(meta)
    }

    pub fn clear_source_watch_override(&self, env_name: &str, token: &str) -> Result<bool, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        let path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        if !path.exists() {
            return Ok(false);
        }
        let existing = match read_json::<SourceWatchOverride>(&path) {
            Ok(existing) => existing,
            Err(_) => {
                remove_file_if_present(&path)?;
                return Ok(true);
            }
        };
        if existing.token != token {
            return Ok(false);
        }
        remove_file_if_present(&path)?;
        Ok(true)
    }

    pub fn active_source_watch_override(
        &self,
        env_name: &str,
    ) -> Result<Option<SourceWatchOverride>, String> {
        match self.inspect_source_watch_state(env_name, true)? {
            SourceWatchState::Active(meta) => Ok(Some(meta)),
            SourceWatchState::Inactive | SourceWatchState::Restoring => Ok(None),
            SourceWatchState::Starting => Err(format!(
                "source watch for env \"{env_name}\" is active or starting, but its metadata is unavailable"
            )),
        }
    }

    pub(crate) fn observe_source_watch(&self, env_name: &str) -> Result<SourceWatchState, String> {
        self.inspect_source_watch_state(env_name, false)
    }

    fn inspect_source_watch_state(
        &self,
        env_name: &str,
        cleanup_stale: bool,
    ) -> Result<SourceWatchState, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        let session = self.source_watch_session(&env_name)?;
        if let Some(session) = &session {
            if let Some(error) = session.unsafe_cleanup_error() {
                return Err(error.to_string());
            }
            #[cfg(unix)]
            if session.requires_controller_completion() && !session.controller_is_running()? {
                return Err(format!(
                    "source controller for env {env_name} exited before cleanup completion; source shutdown is unverified and its unfinished ownership was retained"
                ));
            }
        }
        let path = source_watch_override_path(&env_name, self.env, self.cwd)?;
        let lock_path = path.with_extension("lock");
        let lock_file = match OpenOptions::new().read(true).open(&lock_path) {
            Ok(lock_file) => lock_file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A lookup with no watch state remains read-only. If an override exists,
                // create the lock before cleanup so lease creation cannot interleave.
                if !source_watch_metadata_exists(&path)? {
                    return Ok(SourceWatchState::Inactive);
                }
                if !cleanup_stale {
                    return Ok(read_active_legacy_source_watch(&path, &env_name)?
                        .map(SourceWatchState::Active)
                        .unwrap_or(SourceWatchState::Inactive));
                }
                if let Some(parent) = lock_path.parent() {
                    ensure_dir(parent)?;
                }
                open_source_watch_lock(&lock_path)?
            }
            Err(error) => {
                return Err(format!(
                    "failed opening source watch lock {}: {error}",
                    display_path(&lock_path)
                ));
            }
        };

        #[cfg(windows)]
        if let Some(_lease_event) = open_windows_source_watch_event(&lock_path)? {
            return read_leased_source_watch_state(
                &path,
                &lock_path,
                &env_name,
                cleanup_stale,
                session.as_ref(),
            );
        }

        match try_lock_source_watch_file(&lock_file, false) {
            Ok(()) => {
                // Shared readers prove no watcher owns the exclusive lease. They may clean the
                // same stale metadata concurrently without impersonating an active watcher.
                let legacy = read_active_legacy_source_watch(&path, &env_name);
                let active_legacy = if cleanup_stale {
                    legacy.unwrap_or(None)
                } else {
                    legacy?
                };
                if cleanup_stale && active_legacy.is_none() {
                    remove_file_if_present(&path)?;
                }
                unlock_source_watch_file(&lock_file).map_err(|error| {
                    format!(
                        "failed unlocking stale source watch lock {}: {error}",
                        display_path(&lock_path)
                    )
                })?;
                Ok(active_legacy
                    .map(SourceWatchState::Active)
                    .unwrap_or(SourceWatchState::Inactive))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                read_leased_source_watch_state(
                    &path,
                    &lock_path,
                    &env_name,
                    cleanup_stale,
                    session.as_ref(),
                )
            }
            Err(error) => Err(format!(
                "failed checking source watch lock {}: {error}",
                display_path(&lock_path)
            )),
        }
    }
}

fn read_active_legacy_source_watch(
    path: &Path,
    env_name: &str,
) -> Result<Option<SourceWatchOverride>, String> {
    if !source_watch_metadata_exists(path)? {
        return Ok(None);
    }
    let meta = read_json::<SourceWatchOverride>(path)?;
    Ok(Some(meta).filter(|meta| {
        !is_leased_source_watch(meta)
            && is_valid_source_watch_metadata(meta, env_name)
            && is_legacy_source_watch_process(meta)
    }))
}

fn source_watch_metadata_exists(path: &Path) -> Result<bool, String> {
    path.try_exists().map_err(|error| {
        format!(
            "failed inspecting source watch metadata {}: {error}",
            display_path(path)
        )
    })
}

fn read_leased_source_watch_state(
    path: &Path,
    lock_path: &Path,
    env_name: &str,
    cleanup_stale: bool,
    session: Option<&SourceWatchSession>,
) -> Result<SourceWatchState, String> {
    let lock_lease_id = File::open(lock_path)
        .and_then(|mut file| read_source_watch_lock(&mut file))
        .map_err(|error| {
            format!(
                "failed reading active source watch lock {}: {error}",
                display_path(lock_path)
            )
        })?
        .trim()
        .to_string();
    if source_watch_lock_is_restoring(&lock_lease_id) {
        if cleanup_stale {
            remove_file_if_present(path)?;
        }
        return Ok(SourceWatchState::Restoring);
    }
    if !source_watch_metadata_exists(path)? {
        return Ok(SourceWatchState::Starting);
    }
    let meta = read_json::<SourceWatchOverride>(path).map_err(|error| {
        format!(
            "source watch for env \"{env_name}\" is active or starting, but its metadata is unavailable: {error}"
        )
    })?;
    if source_watch_matches_lease(&meta, &lock_lease_id)
        && is_valid_source_watch_structure(&meta, env_name)
    {
        if meta.ui.is_some() || session.is_some_and(|session| session.ui.is_some()) {
            let matched = session.is_some_and(|session| {
                let Some(ui) = &session.ui else { return false };
                let Some(target) = &ui.target else {
                    return false;
                };
                let Some(endpoint) = &meta.ui else {
                    return false;
                };
                !session.closed
                    && session.lease_id == lock_lease_id
                    && meta.watching == session.watching
                    && target.port == endpoint.port
                    && target.gateway_url == endpoint.gateway_url
                    && ui
                        .children
                        .get(DevUiChildRole::Gateway)
                        .map(|child| child.pid)
                        == Some(meta.watch_pid)
                    && ui.children.get(DevUiChildRole::Ui).map(|child| child.pid)
                        == Some(endpoint.pid)
            });
            if !matched {
                return Err(
                    "dev UI metadata does not match its recorded children and target".to_string(),
                );
            }
        }
        Ok(SourceWatchState::Active(meta))
    } else {
        Err(format!(
            "source watch for env \"{env_name}\" is active or starting, but its metadata does not match the active lease"
        ))
    }
}

fn write_source_watch_lock(lock_file: &mut File, value: &str) -> Result<(), String> {
    #[cfg(windows)]
    if value.len() >= SOURCE_WATCH_GENERATION_MAX_BYTES {
        return Err("source watch generation exceeds its metadata limit".to_string());
    }
    lock_file
        .set_len(0)
        .and_then(|()| lock_file.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| writeln!(lock_file, "{value}"))
        .map_err(|error| format!("failed recording source watch lock: {error}"))
}

fn read_source_watch_lock(lock_file: &mut File) -> io::Result<String> {
    let mut value = String::new();
    #[cfg(not(windows))]
    lock_file.read_to_string(&mut value)?;
    #[cfg(windows)]
    {
        // Bound the ReadFile request itself below the reserved lock byte, even
        // when the file is malformed or the reader's buffer extends beyond EOF.
        lock_file
            .take((SOURCE_WATCH_GENERATION_MAX_BYTES + 1) as u64)
            .read_to_string(&mut value)
            .map_err(|error| {
                if error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION as i32)
                {
                    io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "an older source watch owner locks its generation metadata; stop it from its original terminal",
                    )
                } else {
                    error
                }
            })?;
        if value.len() > SOURCE_WATCH_GENERATION_MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source watch generation exceeds its metadata limit",
            ));
        }
    }
    Ok(value)
}

#[cfg(not(windows))]
fn try_lock_source_watch_file(file: &File, exclusive: bool) -> io::Result<()> {
    if exclusive {
        FileExt::try_lock_exclusive(file)
    } else {
        FileExt::try_lock_shared(file)
    }
}

#[cfg(not(windows))]
fn unlock_source_watch_file(file: &File) -> io::Result<()> {
    FileExt::unlock(file)
}

#[cfg(windows)]
fn source_watch_lock_region() -> windows_sys::Win32::System::IO::OVERLAPPED {
    use windows_sys::Win32::System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};
    OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: SOURCE_WATCH_LOCK_BYTE_OFFSET,
                OffsetHigh: 0,
            },
        },
        ..OVERLAPPED::default()
    }
}

#[cfg(windows)]
fn try_lock_source_watch_file(file: &File, exclusive: bool) -> io::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    let mut region = source_watch_lock_region();
    let flags = LOCKFILE_FAIL_IMMEDIATELY
        | if exclusive {
            LOCKFILE_EXCLUSIVE_LOCK
        } else {
            0
        };
    // The one-byte range overlaps legacy fs2 whole-file locks, but leaves the
    // generation text readable from another handle during a live watch.
    let locked = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut region) };
    if locked != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Err(io::Error::new(io::ErrorKind::WouldBlock, error))
    } else {
        Err(error)
    }
}

#[cfg(windows)]
fn unlock_source_watch_file(file: &File) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    let mut region = source_watch_lock_region();
    if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut region) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn try_lock_source_watch_exclusive(lock_file: &File) -> io::Result<()> {
    for attempt in 0..=SOURCE_WATCH_LOCK_RETRY_ATTEMPTS {
        match try_lock_source_watch_file(lock_file, true) {
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && attempt < SOURCE_WATCH_LOCK_RETRY_ATTEMPTS =>
            {
                thread::sleep(SOURCE_WATCH_LOCK_RETRY_DELAY);
            }
            result => return result,
        }
    }
    unreachable!("bounded source watch lock retry must return")
}

fn source_watch_lock_is_restoring(lock_value: &str) -> bool {
    lock_value.starts_with("restoring:")
}

fn is_leased_source_watch(meta: &SourceWatchOverride) -> bool {
    meta.token.starts_with("lease:")
}

fn source_watch_matches_lease(meta: &SourceWatchOverride, lease_id: &str) -> bool {
    if lease_id.is_empty() {
        return !is_leased_source_watch(meta) && is_process_alive(meta.watch_pid);
    }
    meta.token
        .strip_prefix("lease:")
        .and_then(|token| token.split_once(':'))
        .is_some_and(|(metadata_lease_id, _)| metadata_lease_id == lease_id)
}

fn is_valid_source_watch_metadata(meta: &SourceWatchOverride, env_name: &str) -> bool {
    is_valid_source_watch_structure(meta, env_name) && is_process_alive(meta.watch_pid)
}

fn is_valid_source_watch_structure(meta: &SourceWatchOverride, env_name: &str) -> bool {
    meta.kind == SOURCE_WATCH_OVERRIDE_KIND
        && meta.env_name == env_name
        && !meta.repo_root.trim().is_empty()
        && !meta.token.trim().is_empty()
        && meta.watch_pid > 0
        && meta.endpoint.as_ref().is_none_or(|endpoint| {
            Path::new(&endpoint.env_root).is_absolute()
                && (1..=u16::MAX as u32).contains(&endpoint.gateway_port)
        })
        && Path::new(&meta.repo_root).join("openclaw.mjs").is_file()
        && Path::new(&meta.repo_root).join("extensions").is_dir()
}

#[cfg(windows)]
#[derive(Debug)]
struct WindowsSourceWatchEvent {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for WindowsSourceWatchEvent {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn acquire_windows_source_watch_event(
    lock_path: &Path,
    env_name: &str,
) -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateEventW;

    let event_name = windows_source_watch_event_name(lock_path);
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, event_name.as_ptr()) };
    if event.is_null() {
        return Err(format!(
            "failed creating source watch lease for env \"{env_name}\": {}",
            io::Error::last_os_error()
        ));
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(event);
        }
        return Err(format!(
            "source watch for env \"{env_name}\" is already active or starting"
        ));
    }
    Ok(event)
}

#[cfg(windows)]
fn open_windows_source_watch_event(
    lock_path: &Path,
) -> Result<Option<WindowsSourceWatchEvent>, String> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, GetLastError};
    use windows_sys::Win32::System::Threading::{OpenEventW, SYNCHRONIZATION_SYNCHRONIZE};

    let event_name = windows_source_watch_event_name(lock_path);
    let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, event_name.as_ptr()) };
    if !event.is_null() {
        return Ok(Some(WindowsSourceWatchEvent { handle: event }));
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_FILE_NOT_FOUND {
        Ok(None)
    } else {
        Err(format!(
            "failed checking source watch lease {}: {}",
            display_path(lock_path),
            io::Error::from_raw_os_error(error as i32)
        ))
    }
}

#[cfg(windows)]
fn windows_source_watch_event_name(lock_path: &Path) -> Vec<u16> {
    let digest = Sha256::digest(display_path(lock_path).as_bytes());
    let suffix = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("Local\\OCM-source-watch-{suffix}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn open_source_watch_lock(lock_path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| {
            format!(
                "failed opening source watch lock {}: {error}",
                display_path(lock_path)
            )
        })
}

fn is_legacy_source_watch_process(meta: &SourceWatchOverride) -> bool {
    let command_matches = process_command_line(meta.watch_pid)
        .is_some_and(|command| legacy_source_watch_command_matches(&command));
    if !command_matches {
        return false;
    }
    legacy_source_watch_cwd_matches(meta)
}

fn legacy_source_watch_command_matches(command: &str) -> bool {
    let command = command.replace('\\', "/");
    command.contains("node")
        && command.contains("scripts/watch-node.mjs")
        && command.contains("gateway")
        && command.contains("run")
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed removing {}: {error}", display_path(path))),
    }
}

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_command_line(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "linux")]
fn process_command_line(pid: u32) -> Option<String> {
    let command = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let command = command
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
}

#[cfg(target_os = "linux")]
fn legacy_source_watch_cwd_matches(meta: &SourceWatchOverride) -> bool {
    fs::read_link(format!("/proc/{}/cwd", meta.watch_pid))
        .ok()
        .is_some_and(|cwd| same_path(&cwd, Path::new(&meta.repo_root)))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn legacy_source_watch_cwd_matches(meta: &SourceWatchOverride) -> bool {
    let output = Command::new("lsof")
        .args([
            "-b",
            "-w",
            "-a",
            "-p",
            &meta.watch_pid.to_string(),
            "-d",
            "cwd",
            "-Fn",
        ])
        .output();
    let Ok(output) = output else {
        return false;
    };
    output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix('n'))
            .is_some_and(|cwd| same_path(Path::new(cwd), Path::new(&meta.repo_root)))
}

#[cfg(windows)]
fn legacy_source_watch_cwd_matches(_meta: &SourceWatchOverride) -> bool {
    true
}

#[cfg(unix)]
fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    Command::new("tasklist")
        .args(["/FI", &filter])
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_command_line(pid: u32) -> Option<String> {
    let script = format!("(Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}').CommandLine");
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_watch_override_labels_the_built_entry() {
        let meta = SourceWatchOverride {
            kind: SOURCE_WATCH_OVERRIDE_KIND.to_string(),
            env_name: "demo".to_string(),
            repo_root: "/repo/openclaw".to_string(),
            endpoint: None,
            watch_pid: 123,
            watching: None,
            ui: None,
            token: "123-token".to_string(),
            started_at: OffsetDateTime::UNIX_EPOCH,
        };

        assert_eq!(
            meta.openclaw_entry_path(),
            PathBuf::from("/repo/openclaw/openclaw.mjs")
        );
        assert_eq!(
            meta.extensions_dir(),
            PathBuf::from("/repo/openclaw/extensions")
        );
        assert_eq!(meta.command_label(), "node /repo/openclaw/openclaw.mjs");
    }

    #[test]
    fn legacy_source_watch_identity_requires_the_watch_command() {
        assert!(legacy_source_watch_command_matches(
            "node scripts/watch-node.mjs gateway run --port 18789"
        ));
        assert!(!legacy_source_watch_command_matches(
            "cargo test source_watch"
        ));
    }

    #[test]
    fn exclusive_source_watch_lock_retries_transient_shared_readers() {
        let path = std::env::temp_dir().join(format!(
            "ocm-source-watch-lock-retry-{}-{}",
            std::process::id(),
            now_utc().unix_timestamp_nanos()
        ));
        let reader = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        FileExt::lock_shared(&reader).unwrap();
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release_flag = std::sync::Arc::clone(&released);
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            FileExt::unlock(&reader).unwrap();
            release_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        try_lock_source_watch_exclusive(&contender).unwrap();

        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
        unlock_source_watch_file(&contender).unwrap();
        release.join().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_source_watch_lock_keeps_generation_reads_outside_its_range() {
        let mut owner = tempfile::NamedTempFile::new().unwrap();
        let mut reader = File::open(owner.path()).unwrap();
        try_lock_source_watch_file(owner.as_file(), true).unwrap();
        let generation = "g".repeat(SOURCE_WATCH_GENERATION_MAX_BYTES - 1);
        write_source_watch_lock(owner.as_file_mut(), &generation).unwrap();
        assert_eq!(
            read_source_watch_lock(&mut reader).unwrap().trim(),
            generation
        );
        assert_eq!(
            try_lock_source_watch_file(&reader, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
        );
        write_source_watch_lock(owner.as_file_mut(), "restoring:owned-generation").unwrap();
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(
            read_source_watch_lock(&mut reader).unwrap().trim(),
            "restoring:owned-generation",
        );
        assert!(
            write_source_watch_lock(
                owner.as_file_mut(),
                &"x".repeat(SOURCE_WATCH_GENERATION_MAX_BYTES),
            )
            .is_err()
        );
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(
            read_source_watch_lock(&mut reader).unwrap().trim(),
            "restoring:owned-generation",
        );
        unlock_source_watch_file(owner.as_file()).unwrap();
        try_lock_source_watch_file(&reader, true).unwrap();
        unlock_source_watch_file(&reader).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_source_watch_lock_excludes_legacy_whole_file_owners() {
        let owner = tempfile::NamedTempFile::new().unwrap();
        fs::write(owner.path(), "legacy-generation\n").unwrap();
        let mut contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(owner.path())
            .unwrap();
        FileExt::lock_exclusive(owner.as_file()).unwrap();
        assert_eq!(
            try_lock_source_watch_file(&contender, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
        );
        assert_eq!(
            read_source_watch_lock(&mut contender).unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
        );
        FileExt::unlock(owner.as_file()).unwrap();
        try_lock_source_watch_file(&contender, true).unwrap();
        assert_eq!(
            FileExt::try_lock_shared(owner.as_file())
                .unwrap_err()
                .raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION as i32),
        );
        assert_eq!(
            FileExt::try_lock_exclusive(owner.as_file())
                .unwrap_err()
                .raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION as i32),
        );
        unlock_source_watch_file(&contender).unwrap();
        FileExt::try_lock_exclusive(owner.as_file()).unwrap();
        FileExt::unlock(owner.as_file()).unwrap();
    }
}
