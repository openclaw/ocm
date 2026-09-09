use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use time::OffsetDateTime;

use super::{EnvMeta, EnvironmentService};
#[cfg(unix)]
use crate::infra::process_identity::process_group_members;
use crate::infra::process_identity::{
    ProcessIdentity, current_process_identity, observe_process, process_scope_id,
};
use crate::store::{display_path, source_watch_override_path, validate_name, write_json};

const SESSION_KIND: &str = "ocm-source-watch-session";
const STOP_KIND: &str = "ocm-source-watch-stop";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceWatchSession {
    pub(crate) kind: String,
    pub(crate) env_name: String,
    pub(crate) lease_id: String,
    pub(crate) env_root: String,
    #[serde(with = "time::serde::rfc3339")]
    pub(crate) env_created_at: OffsetDateTime,
    pub(crate) process_scope: Option<String>,
    pub(crate) controller: ProcessIdentity,
    pub(crate) child: Option<ProcessIdentity>,
    pub(crate) child_spawn_pending: bool,
    pub(crate) restore_service: bool,
    #[serde(default)]
    pub(crate) closed: bool,
    #[serde(default)]
    pub(crate) completion: Option<SourceWatchCompletion>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceWatchStopRequest {
    kind: String,
    env_name: String,
    lease_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceWatchCompletion {
    pub(crate) service_restored: bool,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct SourceWatchSessionPaths {
    pub(super) session: PathBuf,
    pub(super) admission: PathBuf,
    request: PathBuf,
}

impl SourceWatchSessionPaths {
    pub(super) fn from_override(path: &Path) -> Self {
        // Distinct terminal extensions keep dotted env names from colliding
        // with another env's override or inherited source-watch lease.
        Self {
            session: path.with_extension("session"),
            admission: path.with_extension("admission"),
            request: path.with_extension("stop"),
        }
    }

    pub(super) fn ensure_previous_session_finished(&self, env_name: &str) -> Result<(), String> {
        match self.load_session(env_name)? {
            Some(session) if !session.closed && !session.can_reclaim_without_restoration()? => {
                Err(format!(
                    "source watch for env \"{env_name}\" has an unfinished session; run `ocm dev stop {env_name}` to recover it before starting again"
                ))
            }
            _ => Ok(()),
        }
    }

    pub(super) fn create_session(
        &self,
        meta: &EnvMeta,
        lease_id: &str,
    ) -> Result<SourceWatchSession, String> {
        let session = SourceWatchSession {
            kind: SESSION_KIND.to_string(),
            env_name: meta.name.clone(),
            lease_id: lease_id.to_string(),
            env_root: meta.root.clone(),
            env_created_at: meta.created_at,
            process_scope: process_scope_id()?,
            controller: current_process_identity()?,
            child: None,
            child_spawn_pending: false,
            restore_service: false,
            closed: false,
            completion: None,
        };
        remove_file_if_present(&self.request)?;
        write_json(&self.session, &session)?;
        Ok(session)
    }

    pub(super) fn load_session(
        &self,
        env_name: &str,
    ) -> Result<Option<SourceWatchSession>, String> {
        let Some(session) = read_optional_json::<SourceWatchSession>(&self.session)? else {
            return Ok(None);
        };
        if session.kind != SESSION_KIND
            || session.env_name != env_name
            || session.lease_id.trim().is_empty()
            || session.controller.pid == 0
            || session.controller.started_at.is_empty()
            || session
                .child
                .as_ref()
                .is_some_and(|child| child.pid == 0 || child.started_at.is_empty())
            || (session.child_spawn_pending && session.child.is_some())
        {
            return Err(format!(
                "source watch ownership metadata for env \"{env_name}\" is invalid"
            ));
        }
        #[cfg(unix)]
        if session.controller.pid > i32::MAX as u32
            || session
                .child
                .as_ref()
                .is_some_and(|child| child.pid <= 1 || child.pid > i32::MAX as u32)
        {
            return Err("source watch ownership contains an invalid process range".to_string());
        }
        Ok(Some(session))
    }

    pub(super) fn save_session(&self, session: &SourceWatchSession) -> Result<(), String> {
        let current = self.load_session(&session.env_name)?.ok_or_else(|| {
            format!(
                "source watch session for env \"{}\" disappeared",
                session.env_name
            )
        })?;
        if current.lease_id != session.lease_id {
            return Err(format!(
                "source watch session for env \"{}\" changed; refusing to replace its ownership metadata",
                session.env_name
            ));
        }
        write_json(&self.session, session)
    }

    pub(super) fn request_stop(&self, session: &SourceWatchSession) -> Result<(), String> {
        // A new request may retry a previous failed cleanup of this generation.
        let mut current = self.load_session(&session.env_name)?.ok_or_else(|| {
            "source watch ownership disappeared before requesting stop".to_string()
        })?;
        if current.lease_id != session.lease_id || current.closed {
            return Err("source watch session changed before requesting stop".to_string());
        }
        if current.completion.take().is_some() {
            self.save_session(&current)?;
        }
        write_json(
            &self.request,
            &SourceWatchStopRequest {
                kind: STOP_KIND.to_string(),
                env_name: session.env_name.clone(),
                lease_id: session.lease_id.clone(),
            },
        )
    }

    pub(super) fn stop_requested(&self, session: &SourceWatchSession) -> Result<bool, String> {
        Ok(
            read_optional_json::<SourceWatchStopRequest>(&self.request)?.is_some_and(|request| {
                request.kind == STOP_KIND
                    && request.env_name == session.env_name
                    && request.lease_id == session.lease_id
            }),
        )
    }

    pub(super) fn completion(
        &self,
        env_name: &str,
        lease_id: &str,
    ) -> Result<Option<SourceWatchCompletion>, String> {
        Ok(self
            .load_session(env_name)?
            .filter(|session| session.lease_id == lease_id)
            .and_then(|session| session.completion))
    }

    pub(super) fn finish(
        &self,
        session: &SourceWatchSession,
        service_restored: bool,
        error: Option<String>,
        preserve_session: bool,
    ) -> Result<(), String> {
        let current = self.load_session(&session.env_name)?.ok_or_else(|| {
            format!(
                "source watch session for env \"{}\" disappeared",
                session.env_name
            )
        })?;
        if current.lease_id != session.lease_id {
            return Err(format!(
                "source watch session for env \"{}\" changed",
                session.env_name
            ));
        }
        if self.stop_requested(session)? {
            remove_file_if_present(&self.request)?;
        }
        let mut completed = session.clone();
        completed.closed = !preserve_session;
        completed.completion = Some(SourceWatchCompletion {
            service_restored,
            error,
        });
        // Completion and ownership closure are one atomic metadata publication.
        self.save_session(&completed)
    }
}

impl SourceWatchSession {
    fn can_reclaim_without_restoration(&self) -> Result<bool, String> {
        if self.restore_service
            || self.process_scope != process_scope_id()?
            || self.controller_is_running()?
        {
            return Ok(false);
        }
        #[cfg(windows)]
        if self.child_spawn_pending {
            return Ok(false);
        }
        if let Some(child) = &self.child {
            if observe_process(child.pid)?
                .is_some_and(|process| process.running && process.identity == *child)
            {
                return Ok(false);
            }
            #[cfg(unix)]
            if !process_group_members(child.pid)?.is_empty() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn controller_is_running(&self) -> Result<bool, String> {
        if self.process_scope != process_scope_id()? {
            return Ok(false);
        }
        Ok(observe_process(self.controller.pid)?
            .is_some_and(|process| process.running && process.identity == self.controller))
    }

    pub(crate) fn restore_target_matches(&self, meta: &EnvMeta) -> bool {
        self.env_name == meta.name
            && self.env_root == meta.root
            && self.env_created_at == meta.created_at
    }
}

impl<'a> EnvironmentService<'a> {
    pub(crate) fn source_watch_session(
        &self,
        env_name: &str,
    ) -> Result<Option<SourceWatchSession>, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        self.source_watch_session_paths(&env_name)?
            .load_session(&env_name)
    }

    pub(crate) fn request_source_watch_stop_locked(
        &self,
        session: &SourceWatchSession,
    ) -> Result<Option<SourceWatchCompletion>, String> {
        let _admission = self.lock_gateway_admission(&session.env_name)?;
        let current = self
            .source_watch_session(&session.env_name)?
            .ok_or_else(|| "source watch session ended before the stop request".to_string())?;
        if current.lease_id != session.lease_id {
            return Err(
                "source watch generation changed; refusing a stale stop request".to_string(),
            );
        }
        if current.closed {
            return current.completion.map(Some).ok_or_else(|| {
                "source watch closed without a verified completion record".to_string()
            });
        }
        let lease = self
            .observe_source_watch_lease(&session.env_name)?
            .ok_or_else(|| {
                "source watch lease is missing; refusing an unverified stop request".to_string()
            })?;
        if lease.lease_id != session.lease_id {
            return Err(
                "source watch generation changed; refusing a stale stop request".to_string(),
            );
        }
        if !lease.held && session.controller_is_running()? {
            return Err("source watch controller no longer holds its recorded lease; refusing an unverified stop request".to_string());
        }
        self.source_watch_session_paths(&session.env_name)?
            .request_stop(session)?;
        Ok(None)
    }

    pub(crate) fn source_watch_completion(
        &self,
        env_name: &str,
        lease_id: &str,
    ) -> Result<Option<SourceWatchCompletion>, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        self.source_watch_session_paths(&env_name)?
            .completion(&env_name, lease_id)
    }

    pub(super) fn source_watch_session_paths(
        &self,
        env_name: &str,
    ) -> Result<SourceWatchSessionPaths, String> {
        let env_name = validate_name(env_name, "Environment name")?;
        Ok(SourceWatchSessionPaths::from_override(
            &source_watch_override_path(&env_name, self.env, self.cwd)?,
        ))
    }
}

fn read_optional_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed reading {}: {error}", display_path(path))),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        format!(
            "invalid source watch metadata at {}: {error}",
            display_path(path)
        )
    })
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed removing {}: {error}", display_path(path))),
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn stop_acknowledges_completed_generation_without_replacing_it() {
        let root = tempfile::tempdir().unwrap();
        let env = BTreeMap::from([("OCM_HOME".to_string(), display_path(root.path()))]);
        let service = EnvironmentService::new(&env, root.path());
        let paths = service.source_watch_session_paths("demo").unwrap();
        fs::create_dir_all(paths.session.parent().unwrap()).unwrap();
        let session = SourceWatchSession {
            kind: SESSION_KIND.to_string(),
            env_name: "demo".to_string(),
            lease_id: "completed-fixture".to_string(),
            env_root: display_path(&root.path().join("env")),
            env_created_at: crate::store::now_utc(),
            process_scope: process_scope_id().unwrap(),
            controller: current_process_identity().unwrap(),
            child: None,
            child_spawn_pending: false,
            restore_service: false,
            closed: false,
            completion: None,
        };
        write_json(&paths.session, &session).unwrap();
        // Stop observed this active generation before its controller completed.
        paths.finish(&session, false, None, false).unwrap();
        let before = fs::read(&paths.session).unwrap();
        let _operation = service.lock_operation("demo").unwrap();
        let completed = service
            .request_source_watch_stop_locked(&session)
            .unwrap()
            .unwrap();
        assert!(!completed.service_restored);
        assert!(completed.error.is_none());
        assert_eq!(fs::read(&paths.session).unwrap(), before);
        assert!(!paths.request.exists());

        let mut stale = session;
        stale.lease_id = "another-generation".to_string();
        assert!(
            service
                .request_source_watch_stop_locked(&stale)
                .unwrap_err()
                .contains("generation changed")
        );
        assert_eq!(fs::read(paths.session).unwrap(), before);
    }
}
