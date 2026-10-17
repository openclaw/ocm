//! Inherited claims are evidence, not children owned by the replacement daemon.
//! The runtime file is canonical so an acknowledgement cannot be resurrected by
//! a later write from a daemon's stale in-memory collections.
use super::*;
use crate::env::EnvMeta;
#[cfg(unix)]
use crate::infra::process_identity::process_group_members;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorRetainedOwnership {
    pub env_name: String,
    pub claim_id: String,
    pub publisher: Option<SupervisorGatewayAdmission>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRecoverySummary {
    pub env_name: String,
    pub claim_token: Option<String>,
    pub publisher: Option<SupervisorGatewayAdmission>,
    pub children: Vec<SupervisorRuntimeChild>,
    pub services: Vec<SupervisorRuntimeService>,
    pub blockers: Vec<String>,
    pub released: bool,
    pub operator_acknowledged: bool,
    pub process_absence_verified: bool,
    pub desired_running: bool,
}

impl SupervisorRuntimeState {
    pub(crate) fn retained_envs(&self) -> BTreeSet<String> {
        self.retained_ownership
            .iter()
            .flatten()
            .map(|claim| claim.env_name.clone())
            .collect()
    }

    fn row_envs(&self) -> BTreeSet<String> {
        self.children
            .iter()
            .map(|r| r.env_name.clone())
            .chain(self.services.iter().map(|r| r.env_name.clone()))
            .collect()
    }

    pub(super) fn validate_ownership(&self, home: &str) -> Result<(), String> {
        if self.kind != SUPERVISOR_RUNTIME_KIND || self.ocm_home != home {
            return Err("supervisor runtime ownership belongs to another store or kind".into());
        }
        let rows = self.row_envs();
        if self.children.iter().any(|row| row.pid == 0)
            || self.services.iter().any(|row| row.pid == Some(0))
        {
            return Err("supervisor ownership contains an invalid child PID".into());
        }
        let mut names = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for claim in self.retained_ownership.iter().flatten() {
            if claim.claim_id.is_empty()
                || !names.insert(&claim.env_name)
                || !ids.insert(&claim.claim_id)
                || !rows.contains(&claim.env_name)
            {
                return Err(
                    "supervisor retained ownership is inconsistent; preserve its record".into(),
                );
            }
        }
        Ok(())
    }

    fn retain_predecessor(&mut self) -> Result<(), String> {
        let mut claims = self.retained_ownership.take().unwrap_or_default();
        for name in self.row_envs() {
            if !claims.iter().any(|claim| claim.env_name == name) {
                claims.push(SupervisorRetainedOwnership {
                    env_name: name,
                    claim_id: supervisor_restart_request_id()?,
                    publisher: self.gateway_admission.clone(),
                });
            }
        }
        self.retained_ownership = Some(claims);
        Ok(())
    }
}

fn read_runtime(path: &Path, home: &str) -> Result<Option<SupervisorRuntimeState>, String> {
    match fs::read(path) {
        Ok(bytes) => {
            let runtime: SupervisorRuntimeState = serde_json::from_slice(&bytes)
                .map_err(|error| format!("cannot read supervisor ownership: {error}"))?;
            runtime.validate_ownership(home)?;
            Ok(Some(runtime))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read supervisor ownership: {error}")),
    }
}

fn lock_runtime(path: &Path) -> Result<crate::store::ExclusiveFileLock, String> {
    // Runtime and desired-state publication share the existing short lock.
    lock_supervisor_state(&path.with_file_name("state.json"))
}

fn publisher_running(publisher: &SupervisorGatewayAdmission) -> Result<bool, String> {
    if publisher.process.pid == 0 || publisher.process.started_at.is_empty() {
        return Err("recorded publisher identity is incomplete; recovery was refused".into());
    }
    if publisher.process_scope != process_scope_id()? {
        return Err(
            "recorded publisher belongs to another process scope; recovery was refused".into(),
        );
    }
    Ok(observe_process(publisher.process.pid)?
        .is_some_and(|p| p.running && p.identity == publisher.process))
}

pub(super) fn write_runtime(path: &Path, mut next: SupervisorRuntimeState) -> Result<(), String> {
    // Process observation is outside the publication lock. Recovery may change
    // the record meanwhile; compare again instead of overwriting that result.
    for _ in 0..3 {
        let before = read_runtime(path, &next.ocm_home)?;
        let replacing = before
            .as_ref()
            .is_some_and(|r| r.gateway_admission != next.gateway_admission);
        if replacing
            && let Some(publisher) = before.as_ref().and_then(|r| r.gateway_admission.as_ref())
            && publisher.process_scope == process_scope_id()?
            && publisher_running(publisher)?
        {
            return Err(
                "another supervisor publisher is still running; its ownership was preserved".into(),
            );
        }
        let _publication = lock_runtime(path)?;
        let current = read_runtime(path, &next.ocm_home)?;
        if current != before {
            continue;
        }
        if let Some(mut previous) = current {
            if replacing {
                previous.retain_predecessor()?;
            }
            let retained = previous.retained_envs();
            if next
                .children
                .iter()
                .any(|child| retained.contains(&child.env_name))
            {
                return Err("cannot replace unresolved supervisor child ownership".into());
            }
            next.services
                .retain(|service| !retained.contains(&service.env_name));
            next.children.extend(
                previous
                    .children
                    .into_iter()
                    .filter(|r| retained.contains(&r.env_name)),
            );
            next.services.extend(
                previous
                    .services
                    .into_iter()
                    .filter(|r| retained.contains(&r.env_name)),
            );
            next.retained_ownership = Some(previous.retained_ownership.unwrap_or_default());
        }
        // A Stop may have removed intent while this daemon still has an old
        // pending plan in memory. Do not recreate a row just acknowledged by
        // recovery. Live and inherited ownership remains independently visible.
        let desired: SupervisorState = read_json(&path.with_file_name("state.json"))?;
        let desired_names: BTreeSet<_> = desired
            .children
            .iter()
            .map(|spec| spec.env_name.as_str())
            .collect();
        let retained = next.retained_envs();
        let unplanned = |row: &SupervisorRuntimeService| {
            row.pid.is_none()
                && !retained.contains(&row.env_name)
                && !desired_names.contains(row.env_name.as_str())
        };
        if next.services.iter().any(unplanned) {
            // A missing binding can remove a plan without changing running
            // intent. Only positive stopped policy permits dropping its row.
            // Admission excludes a policy change across an admitted spawn.
            let store_env = BTreeMap::from([("OCM_HOME".to_string(), next.ocm_home.clone())]);
            let stopped: BTreeSet<_> =
                crate::store::list_environments(&store_env, Path::new(&next.ocm_home))?
                    .into_iter()
                    .filter(|meta| !meta.service_running)
                    .map(|meta| meta.name)
                    .collect();
            next.services
                .retain(|row| !unplanned(row) || !stopped.contains(&row.env_name));
        }
        next.children.sort_by(|a, b| a.env_name.cmp(&b.env_name));
        next.services.sort_by(|a, b| a.env_name.cmp(&b.env_name));
        return write_json(path, &next);
    }
    Err("supervisor ownership kept changing during publication; its record was preserved".into())
}

fn claim_summary(
    runtime: Option<&SupervisorRuntimeState>,
    meta: &EnvMeta,
) -> Result<ServiceRecoverySummary, String> {
    let mut result = ServiceRecoverySummary {
        env_name: meta.name.clone(),
        claim_token: None,
        publisher: None,
        children: Vec::new(),
        services: Vec::new(),
        blockers: Vec::new(),
        released: false,
        operator_acknowledged: false,
        process_absence_verified: false,
        desired_running: meta.service_running,
    };
    let Some(runtime) = runtime else {
        return Ok(result);
    };
    let claim = runtime
        .retained_ownership
        .as_ref()
        .and_then(|claims| claims.iter().find(|claim| claim.env_name == meta.name));
    result.children = runtime
        .children
        .iter()
        .filter(|r| r.env_name == meta.name)
        .cloned()
        .collect();
    result.services = runtime
        .services
        .iter()
        .filter(|r| r.env_name == meta.name)
        .cloned()
        .collect();
    if result.children.is_empty() && result.services.is_empty() {
        return Ok(result);
    }
    result.publisher = claim
        .map(|c| c.publisher.clone())
        .unwrap_or_else(|| runtime.gateway_admission.clone());
    let bytes = serde_json::to_vec(&(
        &runtime.ocm_home,
        &meta.name,
        &meta.root,
        meta.created_at,
        claim,
        &result.publisher,
        &result.children,
        &result.services,
    ))
    .map_err(|error| error.to_string())?;
    result.claim_token = Some(
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    );
    Ok(result)
}

impl SupervisorService<'_> {
    fn ensure_recovery_target_unplanned(&self, name: &str) -> Result<(), String> {
        let path = supervisor_state_path(self.env, self.cwd)?;
        match fs::read(&path) {
            Ok(bytes) => {
                let state: SupervisorState = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("cannot inspect desired supervisor state: {error}"))?;
                if state.kind != SUPERVISOR_STATE_KIND
                    || state.ocm_home != display_path(&resolve_ocm_home(self.env, self.cwd)?)
                {
                    return Err("desired supervisor state belongs to another store or kind".into());
                }
                if state.children.iter().any(|spec| spec.env_name == name) {
                    return Err("target remains in the desired supervisor plan; complete service stop before recovery".into());
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot inspect desired supervisor state: {error}")),
        }
    }

    pub(super) fn ensure_no_retained_ownership(&self, name: &str) -> Result<(), String> {
        let home = display_path(&resolve_ocm_home(self.env, self.cwd)?);
        if let Some(runtime) = read_runtime(&supervisor_runtime_path(self.env, self.cwd)?, &home)? {
            let external = runtime
                .gateway_admission
                .as_ref()
                .is_none_or(|p| p.process.pid != std::process::id());
            if runtime.retained_envs().contains(name)
                || (external && runtime.row_envs().contains(name))
            {
                return Err(format!(
                    "env {name} has unresolved retained service ownership; inspect it with `ocm service recover {name}`"
                ));
            }
        }
        Ok(())
    }

    /// Retained claims remain authoritative even when the native daemon is absent.
    pub(crate) fn ensure_retained_ownership_recovered(&self, name: &str) -> Result<(), String> {
        if self
            .read_runtime_state()?
            .is_some_and(|runtime| runtime.retained_envs().contains(name))
        {
            return Err(format!(
                "env {name} has unresolved retained service ownership; inspect it with `ocm service recover {name}`"
            ));
        }
        Ok(())
    }

    /// Caller holds state publication and target admission through foreground
    /// child ownership publication. Never wait for a writer under these locks.
    pub(crate) fn ensure_foreground_gateway_unowned(&self, name: &str) -> Result<(), String> {
        self.ensure_retained_ownership_recovered(name)?;
        let home = display_path(&resolve_ocm_home(self.env, self.cwd)?);
        if read_runtime(&supervisor_runtime_path(self.env, self.cwd)?, &home)?
            .is_some_and(|runtime| runtime.row_envs().contains(name))
        {
            return Err(format!(
                "background ownership for env {name} has not been retired; foreground Gateway was not started"
            ));
        }
        Ok(())
    }

    fn recovery_writer_check(&self, runtime: &SupervisorRuntimeState) -> Result<(), String> {
        let identity = managed_service_identity(self.env, self.cwd)?;
        match observe_job_for_source_watch(&identity.label, &identity.definition_path, self.env)? {
            ManagedJobObservation::Running(pid) => {
                if runtime.retained_ownership.is_none() {
                    return Err(
                        "the running daemon does not support retained-ownership recovery".into(),
                    );
                }
                if !managed_service_owner_matches(
                    &identity.definition_path,
                    &runtime.ocm_home,
                    self.env,
                )? {
                    return Err("the running daemon belongs to another store".into());
                }
                runtime
                    .gateway_admission
                    .as_ref()
                    .ok_or("the running daemon has no publisher identity")?
                    .verify_process(pid)
            }
            ManagedJobObservation::Absent | ManagedJobObservation::Stopped => {
                let publisher = runtime
                    .gateway_admission
                    .as_ref()
                    .ok_or("runtime has no publisher identity; recovery was refused")?;
                if publisher_running(publisher)? {
                    return Err("runtime publisher is still running despite native absence; recovery was refused".into());
                }
                Ok(())
            }
        }
    }

    fn recovery_blockers(
        &self,
        runtime: Option<&SupervisorRuntimeState>,
        summary: &mut ServiceRecoverySummary,
    ) {
        if summary.desired_running {
            summary
                .blockers
                .push("service intent is running; use ordinary service stop first".into());
        }
        if let Err(error) = self.ensure_recovery_target_unplanned(&summary.env_name) {
            summary.blockers.push(error);
        }
        if let Some(runtime) = runtime
            && let Err(error) = self.recovery_writer_check(runtime)
        {
            summary.blockers.push(error);
        }
        if summary.claim_token.is_none() {
            return;
        }
        match summary.publisher.as_ref().map(publisher_running) {
            None => summary
                .blockers
                .push("claim has no original publisher identity; recovery was refused".into()),
            Some(Ok(true)) => summary
                .blockers
                .push("original publisher is still running; recovery was refused".into()),
            Some(Err(error)) => summary.blockers.push(error),
            Some(Ok(false)) => {}
        }
        let pids: BTreeSet<_> = summary
            .children
            .iter()
            .map(|r| r.pid)
            .chain(summary.services.iter().filter_map(|r| r.pid))
            .collect();
        for pid in pids {
            match observe_process(pid) {
                Ok(Some(p)) if p.running => summary.blockers.push(format!("recorded PID {pid} is occupied; recovery never signals it or assumes its identity")),
                Err(error) => summary.blockers.push(error),
                _ => {}
            }
            #[cfg(unix)]
            match process_group_members(pid) {
                Ok(members) if !members.is_empty() => summary
                    .blockers
                    .push(format!("recorded process group {pid} is still active")),
                Err(error) => summary.blockers.push(error),
                _ => {}
            }
        }
    }

    /// The service layer retains the target operation lock. Inspection is read-only;
    /// an acknowledgement supplies the assertion about unobservable descendants.
    pub(crate) fn recover_ownership(
        &self,
        name: &str,
        expected: Option<&str>,
    ) -> Result<ServiceRecoverySummary, String> {
        let env_service = EnvironmentService::new(self.env, self.cwd);
        let _lifecycle = if expected.is_some() {
            Some(self.lock_daemon_lifecycle()?)
        } else {
            None
        };
        let _admission = if expected.is_some() {
            Some(env_service.lock_gateway_admission(name)?)
        } else {
            None
        };
        let meta = env_service.get(name)?;
        let path = supervisor_runtime_path(self.env, self.cwd)?;
        let home = display_path(&resolve_ocm_home(self.env, self.cwd)?);
        let before = read_runtime(&path, &home)?;
        let mut summary = claim_summary(before.as_ref(), &meta)?;
        if let Err(error) = env_service.ensure_source_watch_allows_state_mutation_locked(name) {
            summary.blockers.push(error);
        }
        self.recovery_blockers(before.as_ref(), &mut summary);
        let Some(expected) = expected else {
            return Ok(summary);
        };
        if summary.claim_token.as_deref() != Some(expected) {
            return Err("retained ownership changed or no longer exists; inspect the current claim before acknowledging".into());
        }
        if !summary.blockers.is_empty() {
            return Err(summary.blockers.join("; "));
        }
        self.validate_daemon_owner_locked()?;
        let _publication = lock_runtime(&path)?;
        self.ensure_recovery_target_unplanned(name)?;
        let current = read_runtime(&path, &home)?;
        let current_meta = env_service.get(name)?;
        let now = claim_summary(current.as_ref(), &current_meta)?;
        if now.claim_token.as_deref() != Some(expected)
            || current_meta.service_running
            || current.as_ref().and_then(|r| r.gateway_admission.as_ref())
                != before.as_ref().and_then(|r| r.gateway_admission.as_ref())
        {
            return Err(
                "retained ownership or publisher changed during recovery; inspect it again".into(),
            );
        }
        let mut current = current.ok_or("retained runtime disappeared during recovery")?;
        if !current.retained_envs().contains(name) {
            current.retain_predecessor()?;
        }
        current.children.retain(|r| r.env_name != name);
        current.services.retain(|r| r.env_name != name);
        current
            .retained_ownership
            .as_mut()
            .expect("retained claims initialized")
            .retain(|claim| claim.env_name != name);
        current.updated_at = now_utc();
        write_json(&path, &current)?;
        summary.released = true;
        summary.operator_acknowledged = true;
        Ok(summary)
    }
}
