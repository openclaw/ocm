//! Asynchronous invocation of the ordinary packaged upgrade owner.
use std::cell::RefCell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{Cli, UpgradeOptions, UpgradeTarget, is_failed_upgrade_outcome};
use crate::infra::process_identity::{
    ProcessIdentity, current_process_identity, observe_process, process_scope_id,
};
use crate::store::{
    lock_file, now_utc, read_json, try_lock_file, upgrade_history_env_dir, validate_name,
    write_json,
};

thread_local! {
    static ACTIVE_JOB: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

struct JobProgress;

impl Drop for JobProgress {
    fn drop(&mut self) {
        ACTIVE_JOB.with(|active| active.borrow_mut().take());
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    env_name: String,
    state: State,
    progress: String,
    revision: u64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    target: UpgradeTarget,
    result: Option<serde_json::Value>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum State {
    Starting,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl State {
    fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Interrupted)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredJob {
    schema: u32,
    job: Job,
    worker: ProcessIdentity,
    process_scope: Option<String>,
    admitted: bool,
    environment_root: String,
    #[serde(with = "time::serde::rfc3339")]
    environment_created_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_binding: Option<ExpectedBinding>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct ExpectedBinding {
    kind: String,
    name: String,
}

impl ExpectedBinding {
    fn parse(value: String) -> Result<Self, String> {
        let (kind, name) = value
            .split_once(':')
            .ok_or("--if-binding requires <kind>:<name>")?;
        if !matches!(kind, "runtime" | "launcher" | "dev" | "none") {
            return Err("--if-binding kind must be runtime, launcher, dev, or none".into());
        }
        Ok(Self {
            kind: kind.into(),
            name: validate_name(name, "Binding name")?,
        })
    }

    fn validate(&self, environment: &crate::env::EnvMeta) -> Result<(), String> {
        let (kind, name) = super::source_binding(environment);
        if self.kind != kind || self.name != name {
            return Err(format!(
                "environment binding changed: expected {}:{}, found {kind}:{name}; no upgrade was started",
                self.kind, self.name,
            ));
        }
        Ok(())
    }
}

impl StoredJob {
    fn matches_environment(&self, environment: &crate::env::EnvMeta) -> bool {
        self.job.env_name == environment.name
            && self.environment_root == environment.root
            && self.environment_created_at == environment.created_at
    }
}

fn advance(job: &mut Job, progress: &str) {
    job.progress = progress.into();
    job.revision += 1;
    job.updated_at = now_utc().max(job.updated_at + time::Duration::milliseconds(1));
}

fn save_job(path: &Path, record: &StoredJob) -> Result<(), String> {
    let parent = path.parent().ok_or("upgrade job has no parent directory")?;
    crate::store::ensure_dir(parent)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".upgrade-job-");
    #[cfg(target_os = "macos")]
    let mut staged = builder
        .make_in(
            parent,
            crate::infra::macos_security::create_private_file_new,
        )
        .map_err(|error| error.to_string())?;
    #[cfg(not(target_os = "macos"))]
    let mut staged = builder
        .tempfile_in(parent)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(&mut staged, record).map_err(|error| error.to_string())?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    staged.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

impl Cli {
    fn upgrade_jobs_dir(&self, name: &str) -> Result<PathBuf, String> {
        let name = validate_name(name, "Environment name")?;
        Ok(upgrade_history_env_dir(&name, &self.env, &self.cwd)?.join("jobs"))
    }

    fn upgrade_job_path(&self, name: &str, id: &str) -> Result<PathBuf, String> {
        let id = validate_name(id, "Upgrade job id")?;
        if id.len() > 128 {
            return Err("upgrade job id must be at most 128 characters".into());
        }
        Ok(self.upgrade_jobs_dir(name)?.join(format!("{id}.json")))
    }

    fn read_upgrade_job(&self, name: &str, id: &str) -> Result<StoredJob, String> {
        let record: StoredJob = read_json(&self.upgrade_job_path(name, id)?)?;
        if record.schema != 1 || record.job.id != id || record.job.env_name != name {
            return Err("unsupported or mismatched upgrade job record".into());
        }
        Ok(record)
    }

    fn observe_upgrade_job(&self, name: &str, id: &str) -> Result<Job, String> {
        let mut record = self.read_upgrade_job(name, id)?;
        if matches!(record.job.state, State::Starting | State::Running)
            && (record.process_scope != process_scope_id()?
                || !observe_process(record.worker.pid)?
                    .is_some_and(|process| process.running && process.identity == record.worker))
            && let Some(_claim) = try_lock_file(
                &self
                    .upgrade_job_path(name, id)?
                    .with_extension("worker.lock"),
                "upgrade job worker",
            )?
        {
            // A worker may have claimed or finished between observation and lock.
            record = self.read_upgrade_job(name, id)?;
            if matches!(record.job.state, State::Starting | State::Running)
                && (record.process_scope != process_scope_id()?
                    || !observe_process(record.worker.pid)?.is_some_and(|process| {
                        process.running && process.identity == record.worker
                    }))
            {
                record.job.state = State::Interrupted;
                record.job.error = Some(
                    "upgrade worker exited without a result; recovery is unresolved; inspect the environment before retrying".into(),
                );
                advance(&mut record.job, "Worker interrupted; recovery unresolved");
                save_job(&self.upgrade_job_path(name, id)?, &record)?;
            }
        }
        Ok(record.job)
    }

    pub(in crate::cli) fn handle_upgrade_job(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, _json) = Self::consume_flag(args, "--json");
        let Some(action) = args.first().map(String::as_str) else {
            return Err("upgrade job requires capabilities, start, or status".into());
        };
        match action {
            "capabilities" => {
                let [_, name] = args.as_slice() else {
                    return Err("upgrade job capabilities requires <env>".into());
                };
                let meta = self.environment_service().get(name)?;
                let paths = crate::store::derive_env_paths(&meta.root);
                let (binding_kind, binding_name) = super::source_binding(&meta);
                self.print_json(&serde_json::json!({
                    "protocol": "ocm.upgrade-job",
                    "protocolVersion": 1,
                    "supported": cfg!(unix),
                    "envName": meta.name,
                    "envRoot": paths.root,
                    "stateDir": paths.state_dir,
                    "configPath": paths.config_path,
                    "selectors": ["version", "channel", "runtime"],
                    "operations": ["packaged-upgrade"],
                    "bindingKind": binding_kind,
                    "bindingName": binding_name,
                }))?;
            }
            "status" => {
                let (args, id) = Self::consume_option(args[1..].to_vec(), "--request-id")?;
                let id = Self::require_option_value(id, "--request-id")?;
                let [name] = args.as_slice() else {
                    return Err("upgrade job status requires <env> [--request-id <id>]".into());
                };
                let normalized = validate_name(name, "Environment name")?;
                let name = normalized.as_str();
                let id = match id {
                    Some(id) => id,
                    None => {
                        let environment = self.environment_service().get(name)?;
                        let latest = self.upgrade_jobs_dir(name)?.join("latest");
                        if !latest.try_exists().map_err(|error| error.to_string())? {
                            self.print_json(&Option::<Job>::None)?;
                            return Ok(0);
                        }
                        let id: String = read_json(&latest)?;
                        let record = self.read_upgrade_job(name, &id)?;
                        if !record.matches_environment(&environment) {
                            if !self.observe_upgrade_job(name, &id)?.state.terminal() {
                                return Err(format!(
                                    "prior environment instance still has active upgrade job {id}; inspect `ocm upgrade job status {name} --request-id {id}` before further changes"
                                ));
                            }
                            self.print_json(&Option::<Job>::None)?;
                            return Ok(0);
                        }
                        id
                    }
                };
                self.print_json(&self.observe_upgrade_job(name, &id)?)?;
            }
            "start" => {
                let (args, id) = Self::consume_option(args[1..].to_vec(), "--request-id")?;
                let id = Self::require_option_value(id, "--request-id")?;
                let (args, binding) = Self::consume_option(args, "--if-binding")?;
                let binding = Self::require_option_value(binding, "--if-binding")?
                    .map(ExpectedBinding::parse)
                    .transpose()?;
                let (args, target) = UpgradeTarget::parse(args)?;
                let [name] = args.as_slice() else {
                    return Err("upgrade job start requires <env> [--version <version> | --channel <channel> | --runtime <runtime>]".into());
                };
                self.start_upgrade_job(name, target, id, binding)?;
            }
            _ => return Err(format!("unknown upgrade job action: {action}")),
        }
        Ok(0)
    }

    fn start_upgrade_job(
        &self,
        name: &str,
        target: UpgradeTarget,
        requested_id: Option<String>,
        expected_binding: Option<ExpectedBinding>,
    ) -> Result<(), String> {
        let requested_id = requested_id
            .map(|id| validate_name(&id, "Upgrade job id"))
            .transpose()?;
        let normalized = validate_name(name, "Environment name")?;
        let name = normalized.as_str();
        let environment = self.environment_service().get(name)?;
        let root = self.upgrade_jobs_dir(name)?;
        let admission = try_lock_file(&root.join("admission.lock"), "upgrade job admission")?;
        if let Some(id) = requested_id.as_deref()
            && self
                .upgrade_job_path(name, id)?
                .try_exists()
                .map_err(|error| error.to_string())?
        {
            let previous = self.read_upgrade_job(name, id)?;
            if !previous.matches_environment(&environment) {
                return Err(
                    "upgrade request id belongs to a different environment instance".into(),
                );
            }
            if previous.job.target != target {
                return Err("upgrade request id already belongs to a different target".into());
            }
            if previous.expected_binding != expected_binding {
                return Err(
                    "upgrade request id already belongs to a different binding condition".into(),
                );
            }
            self.print_json(&self.observe_upgrade_job(name, id)?)?;
            return Ok(());
        }
        let _admission = admission.ok_or_else(|| {
            format!("an upgrade job is active for {name}; inspect `ocm upgrade job status {name}`")
        })?;
        let environment = self.environment_service().get(name)?;
        if let Some(binding) = &expected_binding {
            binding.validate(&environment)?;
        }
        if root.join("latest").exists() {
            let id: String = read_json(&root.join("latest"))?;
            let previous = self.observe_upgrade_job(name, &id)?;
            if !previous.state.terminal() {
                return Err(format!(
                    "upgrade job {} is {:?}; inspect `ocm upgrade job status {name} --request-id {}` before further changes",
                    previous.id, previous.state, previous.id,
                ));
            }
        }
        let created_at = now_utc();
        let id = requested_id.unwrap_or_else(|| {
            format!(
                "{}-{}",
                std::process::id(),
                created_at.unix_timestamp_nanos()
            )
        });
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command =
            crate::cli::detached_worker::command(&executable, &format!("ocm-upgrade-{id}"))?;
        let record = StoredJob {
            schema: 1,
            job: Job {
                id: id.clone(),
                env_name: name.into(),
                state: State::Starting,
                progress: "Starting upgrade worker".into(),
                revision: 0,
                created_at,
                updated_at: created_at,
                target,
                result: None,
                error: None,
            },
            worker: current_process_identity()?,
            process_scope: process_scope_id()?,
            admitted: false,
            environment_root: environment.root,
            environment_created_at: environment.created_at,
            expected_binding,
        };
        let path = self.upgrade_job_path(name, &id)?;
        save_job(&path, &record)?;
        write_json(&root.join("latest"), &id)?;
        let spawned = command
            .args(["__daemon", "upgrade-job", name, &id])
            .env_clear()
            .envs(&self.env)
            .env_remove("OCM_ACTIVE_ENV")
            .env_remove("OPENCLAW_SERVICE_KIND")
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let mut record = record;
                record.job.state = State::Failed;
                record.job.error = Some(format!("cannot start upgrade worker: {error}"));
                advance(&mut record.job, "Worker startup failed");
                save_job(&path, &record)?;
                return Err(record.job.error.unwrap());
            }
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut record = self.read_upgrade_job(name, &id)?;
            if record.job.state != State::Starting {
                // Unlocking also happens on caller death or errors, so an explicit
                // commit after flushing the response authorizes upgrade work.
                let mut output = io::stdout().lock();
                let response =
                    serde_json::to_string_pretty(&record.job).map_err(|error| error.to_string())?;
                writeln!(output, "{response}")
                    .and_then(|()| output.flush())
                    .map_err(|error| format!("cannot return upgrade request id: {error}"))?;
                record.admitted = true;
                save_job(&path, &record)?;
                return Ok(());
            }
            if child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
                || Instant::now() >= deadline
            {
                return Err(format!(
                    "upgrade job {id} did not acknowledge startup; inspect `ocm upgrade job status {name} --request-id {id}` before retrying"
                ));
            }
            sleep(Duration::from_millis(25));
        }
    }

    pub(in crate::cli) fn run_upgrade_job(&self, args: Vec<String>) -> Result<i32, String> {
        let [name, id] = args.as_slice() else {
            return Err("upgrade worker requires an environment and request id".into());
        };
        let mut record = self.read_upgrade_job(name, id)?;
        if record.job.state != State::Starting {
            return Err("upgrade job has already been claimed".into());
        }
        // Claim once before publishing readiness, including duplicate internal invocations.
        let path = self.upgrade_job_path(name, id)?;
        let _claim = try_lock_file(&path.with_extension("worker.lock"), "upgrade job worker")?
            .ok_or("upgrade job already has a worker")?;
        record = self.read_upgrade_job(name, id)?;
        if record.job.state != State::Starting {
            return Err("upgrade job has already been claimed".into());
        }
        record.worker = current_process_identity()?;
        record.process_scope = process_scope_id()?;
        record.job.state = State::Running;
        advance(&mut record.job, "Waiting for upgrade admission");
        save_job(&path, &record)?;
        let _admission = lock_file(
            &self.upgrade_jobs_dir(name)?.join("admission.lock"),
            "upgrade job admission",
        )?;
        record = self.read_upgrade_job(name, id)?;
        if !record.admitted {
            record.job.state = State::Failed;
            record.job.error =
                Some("requester exited before committing the job; no upgrade was started".into());
            advance(&mut record.job, "Request not admitted");
            save_job(&path, &record)?;
            return Ok(1);
        }
        ACTIVE_JOB.with(|active| *active.borrow_mut() = Some((name.clone(), id.clone())));
        let _progress = JobProgress;
        self.upgrade_job_progress("Upgrading environment")?;
        let result = self.upgrade_env(
            name,
            &record.job.target,
            UpgradeOptions {
                dry_run: false,
                rollback_enabled: true,
            },
        );
        record = self.read_upgrade_job(name, id)?;
        match result {
            Ok(summary) => {
                record.job.state = if is_failed_upgrade_outcome(&summary.outcome) {
                    State::Failed
                } else {
                    State::Succeeded
                };
                match serde_json::to_value(summary) {
                    Ok(result) => record.job.result = Some(result),
                    Err(error) => {
                        record.job.state = State::Failed;
                        record.job.error = Some(format!(
                            "upgrade finished but its result could not be encoded: {error}"
                        ));
                    }
                }
            }
            Err(error) => {
                record.job.state = State::Failed;
                record.job.error = Some(error);
            }
        }
        advance(&mut record.job, "Finished");
        save_job(&path, &record)?;
        Ok(if record.job.state == State::Succeeded {
            0
        } else {
            1
        })
    }

    // Called by the ordinary upgrade owner while its environment operation lock is held.
    pub(super) fn validate_upgrade_job_environment(&self, name: &str) -> Result<(), String> {
        let Some((job_env, id)) = ACTIVE_JOB.with(|active| active.borrow().clone()) else {
            return Ok(());
        };
        let record = self.read_upgrade_job(&job_env, &id)?;
        let current = self.environment_service().get(name)?;
        if !record.matches_environment(&current) {
            return Err(
                "environment was replaced after upgrade job admission; no upgrade was started"
                    .into(),
            );
        }
        if let Some(binding) = &record.expected_binding {
            binding.validate(&current)?;
        }
        Ok(())
    }

    pub(in crate::cli) fn upgrade_job_progress(&self, message: &str) -> Result<(), String> {
        let Some((name, id)) = ACTIVE_JOB.with(|active| active.borrow().clone()) else {
            return Ok(());
        };
        let mut record = self.read_upgrade_job(&name, &id)?;
        if record.worker != current_process_identity()?
            || record.process_scope != process_scope_id()?
        {
            return Err("upgrade progress does not belong to this worker".into());
        }
        advance(&mut record.job, message);
        save_job(&self.upgrade_job_path(&name, &id)?, &record)
    }
}
