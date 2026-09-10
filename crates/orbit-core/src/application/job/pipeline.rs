use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(unix))]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use chrono::Utc;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{
    AuditEventInsertParams, ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams,
    JobRunStepParams, TaskReservationReleaseReason,
};
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    JobRun, JobRunStartOutcome, JobRunState, JobScheduleState, JobTargetType,
};
use orbit_types::workspace::WorkspacePaths;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use orbit_engine::activity_job::load_job_asset;
use orbit_types::workflow::JobV2;
use orbit_types::workflow::activity_job::{
    TRUSTED_HOST_ADMISSION_KEY, run_input_declares_trusted_host, validate_job_retired_sessions,
};

use orbit_types::workflow::OPERATION_ADMISSION_KEY;

use crate::OrbitRuntime;
use crate::application::job::exec::V2RunFinalizationOptions;
use crate::application::job::resume::ResumePlan;
use crate::application::operation::{
    child_admission_authority, inherit_child_admission, reserved_operation_key_error,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

#[cfg(unix)]
use super::run::CANCELLATION_WORKER_EXIT_AUDIT;

const PIPELINE_WAIT_DEFAULT_TIMEOUT_SECONDS: u64 = 3600;
const PIPELINE_WAIT_MAX_TIMEOUT_SECONDS: u64 = 7200;
const PIPELINE_WAIT_DEFAULT_POLL_SECONDS: u64 = 5;
const PIPELINE_WAIT_MIN_POLL_SECONDS: u64 = 1;
const PIPELINE_WORKER_LOG_TAIL_BYTES: u64 = 16 * 1024;
/// Run-input field carrying a caller's agent-invocation retry key [ORB-11354].
const AGENT_INVOKE_IDEMPOTENCY_KEY_FIELD: &str = "idempotency_key";
/// Cap on the run history scanned when matching an agent-invocation retry key.
const AGENT_INVOKE_IDEMPOTENCY_SCAN_LIMIT: usize = 200;

/// [ORB-10544] Cap on the run history scanned by the in-flight ship guard.
/// Non-terminal runs are always among the newest rows, so a bounded window is
/// enough to spot a duplicate dispatch without walking the whole history.
const SHIP_IN_FLIGHT_SCAN_LIMIT: usize = 200;

/// [ORB-11998] Run-input field carrying the owning workspace's `.orbit`
/// directory, as set by routine dispatch (`RuntimeDispatch::submit`). The
/// executing worker verifies its own resolved workspace against this value
/// before running any step, so a workspace-routing failure surfaces as a
/// visibly failed run instead of a silent no-op success.
pub(crate) const ROUTINE_DISPATCH_ORBIT_DIR_FIELD: &str = "__routine_dispatch_orbit_dir";

/// [ORB-12038] `error_code` recorded on the diagnostic step for a routine-
/// dispatch workspace mismatch, so `orbit run show` names the cause rather
/// than an operator finding only a bare `cancelled` state.
pub(crate) const ROUTINE_DISPATCH_WORKSPACE_MISMATCH_ERROR_CODE: &str =
    "routine_dispatch_workspace_mismatch";

/// The refusal for a submission that supplied the reserved trusted-host
/// admission key it is not entitled to write [ORB-11354].
///
/// Shared by every entry point that accepts caller-shaped run input so the
/// refusal reads identically whether it came from `orbit run job`, a direct
/// YAML path, a resume, or a tool call.
pub(crate) fn reserved_trusted_host_key_error(job_name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "run input for job '{job_name}' set the reserved `{TRUSTED_HOST_ADMISSION_KEY}` field; \
         trusted host execution is admitted per invocation by the governed `orbit.agent.invoke` \
         operation and cannot be requested through ordinary job input"
    ))
}

/// Whether caller-shaped run input names the reserved operation-mode
/// admission key [ORB-11332].
fn run_input_declares_operation_admission(input: &Value) -> bool {
    input
        .get(OPERATION_ADMISSION_KEY)
        .is_some_and(|value| !value.is_null())
}

/// One durable pipeline submission: what to run, with what input, and how the
/// detached worker will find the definition again.
struct PipelineSubmission<'a> {
    job_name: &'a str,
    definition: SubmittedDefinition<'a>,
    input: Value,
    resume: Option<&'a ResumePlan>,
    actor: Option<&'a str>,
    action_key: Option<&'a str>,
    /// Whether this submission is the canonical trusted-host admission
    /// [ORB-11354]. Only it may carry [`TRUSTED_HOST_ADMISSION_KEY`] in its
    /// input; every other submission is refused for supplying it.
    trusted_host: bool,
    /// Whether this submission is the grant-bound drain coordinator
    /// [ORB-11332]. Only it (and the parent-authorized child path, which
    /// copies the parent's snapshot) may carry [`OPERATION_ADMISSION_KEY`].
    operation_bound: bool,
}

/// What a parent-authorized child submission produced.
#[derive(Debug, Clone)]
pub(crate) enum ChildSubmission {
    Submitted(PipelineInvokeResult),
    /// The atomic admission refused the child; `reason` is
    /// `admissions_stopped` or one of the grant-bound refusals.
    Skipped(String),
}

impl ChildSubmission {
    fn run_id(&self) -> Option<&str> {
        match self {
            ChildSubmission::Submitted(result) => Some(result.run_id.as_str()),
            ChildSubmission::Skipped(_) => None,
        }
    }
}

impl<'a> PipelineSubmission<'a> {
    /// An ordinary submission: catalog definition, no resume, no idempotency
    /// key, and no trusted-host admission.
    fn catalog(job_name: &'a str, input: Value, actor: Option<&'a str>) -> Self {
        Self {
            job_name,
            definition: SubmittedDefinition::Catalog,
            input,
            resume: None,
            actor,
            action_key: None,
            trusted_host: false,
            operation_bound: false,
        }
    }
}

/// Trusted context for a pipeline child submitted by a running v2 activity.
///
/// The parent run id comes from the engine-owned [`orbit_tools::ToolContext`],
/// never from tool input. The remaining fields make the parent link complete
/// at the same atomic boundary that creates the child.
#[derive(Debug, Clone)]
pub(crate) struct ChildPipelineAdmission {
    pub parent_run_id: String,
    pub parent_step_id: Option<String>,
    pub action: String,
    pub blocking: bool,
}

/// How a submitted run's definition reaches its worker.
enum SubmittedDefinition<'a> {
    /// Resolve `job_name` from the catalog, at submission and again in the
    /// worker. Catalog assets are managed, so name resolution stays the
    /// contract for them.
    Catalog,
    /// Pin this exact validated YAML alongside the run [ORB-10801]. A direct
    /// path is an unmanaged file the submitter happened to name; rereading it
    /// from a detached worker would let an edit or deletion between submission
    /// and execution change (or destroy) the run.
    Snapshot { spec: &'a JobV2, yaml: &'a str },
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineInvokeResult {
    pub run_id: String,
    pub job_name: String,
    pub submitted_at: String,
    pub queued: bool,
}

/// [ORB-12038] A run's own `<run_id>.worker.log`, read for inspection when no
/// audited CLI-invocation blob exists to explain a terminal outcome. See
/// [`OrbitRuntime::read_pipeline_worker_log`].
#[derive(Debug, Clone, Serialize)]
pub struct PipelineWorkerLogSnapshot {
    pub path: PathBuf,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineWaitResult {
    pub results: Vec<PipelineWaitEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineWaitEntry {
    pub run_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl OrbitRuntime {
    /// Submit a `ship` workflow run (the `task_auto_pipeline` job).
    ///
    /// Shared entry point for every non-interactive submission surface
    /// (dashboard HTTP endpoint, MCP `orbit.workflow.ship`, `orbit run
    /// ship-sweep`). `base_branch` falls back to the workspace's `[workflow]
    /// base_branch`; an empty `task_ids` slice selects auto
    /// (backlog-discovery) mode. One-shot: returns as soon as the run is
    /// persisted and its worker spawned.
    ///
    /// [ORB-10544] An explicit task selection is guarded against duplicate
    /// dispatch here rather than in any one adapter: if a named task is already
    /// carried by a non-terminal run, the submission is refused with
    /// [`OrbitError::ShipRunInFlight`] naming that task and run, so two runs
    /// cannot contend for one worktree and task reservation no matter which
    /// surface submitted them. Auto mode has no task ids to key on and is
    /// unaffected.
    ///
    /// [ORB-11187] `completion` is the caller's explicit authorization for this
    /// run to finish delivery and perform the guarded `review -> done`
    /// transition. It defaults to
    /// [`CompletionPolicy::Review`](crate::application::workflow::CompletionPolicy::Review)
    /// at every surface and is only ever raised by a per-invocation operator
    /// flag; nothing derives it from workspace configuration or the environment.
    ///
    /// [ORB-10709] The workspace claim is checked first, for the case the
    /// duplicate-dispatch guard structurally cannot cover: it is keyed on task
    /// id over a bounded window of recent runs, so a stale non-terminal run
    /// outside that window is invisible to it, and a discovery-mode submission
    /// carries no task ids at all. The claim check is keyed on neither, so both
    /// gaps close. `claim_token` is the holder's minted token; `None` falls back
    /// to [`CLAIM_TOKEN_ENV`](crate::runtime::workspace_claim::CLAIM_TOKEN_ENV).
    // Existing public positional API; keep callers stable while submission is composed internally.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_ship_run(
        &self,
        mode: crate::application::workflow::ShipMode,
        base_branch: Option<&str>,
        task_ids: &[String],
        completion: crate::application::workflow::CompletionPolicy,
        allowed_crews: &[String],
        actor: Option<&str>,
        claim_token: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.ship", claim_token)?;
        let workflow = crate::application::workflow::find_workflow(
            crate::application::workflow::SHIP_WORKFLOW_ALIAS,
        )
        .ok_or_else(|| OrbitError::InvalidInput("unknown workflow 'ship'".to_string()))?;
        let base = base_branch.unwrap_or_else(|| self.workflow_base_branch());
        let allowed_crews = self.canonical_allowed_crews(allowed_crews)?;
        let allowlist = self.crew_allowlist(&allowed_crews)?;
        let input = crate::application::workflow::build_ship_input(
            mode,
            base,
            task_ids,
            completion,
            &allowed_crews,
        )?;
        // Validate explicit selections before inspecting runs or creating a
        // pipeline record. Auto mode intentionally carries no task ids: the
        // worker discovers eligible backlog tasks after it starts.
        for task_id in task_ids {
            let task = self.get_task(task_id)?;
            if task.tags.iter().any(|tag| tag == "epic") {
                return Err(OrbitError::InvalidInput(format!(
                    "task '{task_id}' is an epic root and cannot be shipped as a leaf; use `orbit run auto` or `orbit run job epic_pipeline`"
                )));
            }
            if let Some(allowlist) = allowlist.as_ref() {
                let crew = self.effective_task_crew(&task)?;
                crate::runtime::engine::crew::enforce_crew_allowlist(
                    Some(allowlist),
                    &crew,
                    &format!("explicit ship task '{task_id}'"),
                )?;
            }
        }
        if let Some(conflict) = self.in_flight_ship_run_for_tasks(task_ids)? {
            return Err(conflict);
        }
        self.submit_pipeline_run(workflow.job_id, input, None, actor)
    }

    /// Submit one workspace drain (`workspace_auto_pipeline`).
    ///
    /// [ORB-10819] `for_seconds` is the drain window: the run keeps re-listing
    /// admissible work and shipping it until the window expires. `None` (or
    /// zero) means one tick, which is what every caller predating the window
    /// gets. The window bounds only the *start* of new work — a child run
    /// already in flight when the deadline passes finishes normally.
    /// `max_active_leaf_runs` is the drain's concurrency ceiling: how many
    /// `task_auto_pipeline` children may be live at once. Omitted, the job's
    /// own default applies — this only forwards an explicit override, so the
    /// default lives in one place, next to the loop that reads it.
    ///
    /// [ORB-11242] `allowed_crews` is the run-scoped crew restriction. Empty
    /// means unrestricted, which is what every caller predating it gets. Names
    /// are resolved against this host's `[crews.*]` registry *here*, before a
    /// run record exists, so an unknown or blank name fails the submission
    /// rather than quietly shrinking what a live drain admits; the canonical
    /// registry names are what gets persisted and forwarded. It gates what the
    /// drain may *start* — it does not touch workspace configuration, reassign
    /// a task's crew, or cancel work another invocation already has in flight.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_workspace_auto_run(
        &self,
        for_seconds: Option<u64>,
        max_active_leaf_runs: Option<u32>,
        completion: crate::application::workflow::CompletionPolicy,
        allowed_crews: &[String],
        complexity_crews: &orbit_config::ComplexityCrewPools,
        actor: Option<&str>,
        claim_token: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.auto", claim_token)?;
        let workflow = crate::application::workflow::find_workflow(
            crate::application::workflow::AUTO_WORKFLOW_ALIAS,
        )
        .ok_or_else(|| OrbitError::InvalidInput("unknown workflow 'auto'".to_string()))?;
        let mut input = workspace_auto_run_input(
            for_seconds,
            max_active_leaf_runs,
            completion,
            &self.canonical_allowed_crews(allowed_crews)?,
        )?;
        Self::set_auto_crew_overrides(&mut input, complexity_crews);
        self.submit_pipeline_run(workflow.job_id, input, None, actor)
    }

    /// Canonicalize an operator-supplied crew allowlist, rejecting blank or
    /// unconfigured names [ORB-11242].
    ///
    /// Canonical registry names are persisted rather than the operator's
    /// spelling, so the durable run input says exactly which configured crews
    /// the window permits regardless of the alias that was typed.
    pub(crate) fn canonical_allowed_crews(
        &self,
        allowed_crews: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        let mut canonical: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for name in allowed_crews {
            if name.trim().is_empty() {
                return Err(OrbitError::InvalidInput(
                    "crew name in the allowlist must not be empty".to_string(),
                ));
            }
            let Some(resolved) = self.canonical_crew_name(Some(name))? else {
                return Err(OrbitError::InvalidInput(
                    "crew name in the allowlist must not be empty".to_string(),
                ));
            };
            canonical.insert(resolved);
        }
        Ok(canonical.into_iter().collect())
    }

    /// The duplicate-dispatch refusal for the newest non-terminal run already
    /// carrying one of `task_ids`, or `None` when the selection is free.
    ///
    /// A run's task selection lives in its persisted `input.task_ids`, which is
    /// what [`Self::submit_ship_run`] writes, so this sees every prior
    /// submission regardless of the surface that made it.
    fn in_flight_ship_run_for_tasks(
        &self,
        task_ids: &[String],
    ) -> Result<Option<OrbitError>, OrbitError> {
        if task_ids.is_empty() {
            return Ok(None);
        }
        let runs = self.list_job_runs(crate::application::job::JobRunListParams {
            limit: Some(SHIP_IN_FLIGHT_SCAN_LIMIT),
            ..Default::default()
        })?;
        Ok(runs.into_iter().find_map(|run| {
            if run.state.is_terminal() {
                return None;
            }
            let task_id = run
                .input
                .as_ref()
                .and_then(|input| input.get("task_ids"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|candidate| task_ids.iter().any(|wanted| wanted == candidate))
                .map(str::to_string)?;
            Some(OrbitError::ShipRunInFlight {
                task_id,
                run_id: run.run_id,
            })
        }))
    }

    /// Persist and dispatch one operator-admitted trusted-host invocation
    /// [ORB-11354].
    ///
    /// The only submission permitted to write [`TRUSTED_HOST_ADMISSION_KEY`],
    /// which is why it is here — on the module that owns the refusal — rather
    /// than assembling a `PipelineSubmission` from outside.
    ///
    /// `idempotency_key` makes a retried submission resolve the run the first
    /// attempt created instead of starting a second subprocess. Keys are
    /// matched over a bounded window of this job's recent runs, the same shape
    /// the ship guard uses: a key older than that window is not recognized and
    /// submits again, which is why a key is a retry handle rather than a
    /// permanent uniqueness constraint.
    pub(super) fn submit_trusted_host_pipeline_run(
        &self,
        input: Value,
        actor: &str,
        idempotency_key: Option<&str>,
    ) -> Result<(PipelineInvokeResult, bool), OrbitError> {
        let job_name = crate::application::job::AGENT_INVOKE_JOB_ID;
        let idempotency_key = idempotency_key
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut input = input;
        if let Some(key) = idempotency_key {
            if let Some(existing) = self.agent_invoke_run_for_key(job_name, key)? {
                return Ok((
                    PipelineInvokeResult {
                        run_id: existing.run_id,
                        job_name: job_name.to_string(),
                        submitted_at: existing.scheduled_at.to_rfc3339(),
                        queued: existing.state == JobRunState::Pending,
                    },
                    true,
                ));
            }
            if let Some(object) = input.as_object_mut() {
                object.insert(
                    AGENT_INVOKE_IDEMPOTENCY_KEY_FIELD.to_string(),
                    Value::String(key.to_string()),
                );
            }
        }
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            trusted_host: true,
            ..PipelineSubmission::catalog(job_name, input.clone(), Some(actor))
        });
        self.record_pipeline_audit(
            "agent.invoke",
            result.as_ref().ok().map(|value| value.run_id.as_str()),
            Some(actor),
            match &result {
                Ok(_) => AuditEventStatus::Success,
                Err(_) => AuditEventStatus::Failure,
            },
            json!({
                "actor": actor,
                "job_name": job_name,
                "run_id": result.as_ref().ok().map(|value| value.run_id.clone()),
                "idempotency_key": idempotency_key,
                "input_hash": input_hash(&input),
            }),
            result.as_ref().err().map(|error| error.to_string()),
        )?;
        result.map(|invoke| (invoke, false))
    }

    /// The newest recent run of `job_name` submitted under `key`, if any.
    fn agent_invoke_run_for_key(
        &self,
        job_name: &str,
        key: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        let runs = self.list_job_runs(crate::application::job::JobRunListParams {
            job_id: Some(job_name.to_string()),
            limit: Some(AGENT_INVOKE_IDEMPOTENCY_SCAN_LIMIT),
            ..Default::default()
        })?;
        Ok(runs.into_iter().find(|run| {
            run.input
                .as_ref()
                .and_then(|input| input.get(AGENT_INVOKE_IDEMPOTENCY_KEY_FIELD))
                .and_then(Value::as_str)
                == Some(key)
        }))
    }

    /// [ORB-10470] Submit a resume of a terminal run as a detached run.
    ///
    /// The non-blocking counterpart to
    /// [`OrbitRuntime::resume_job_run`](crate::OrbitRuntime::resume_job_run):
    /// it persists the resumed run (seeded with the source's checkpoints),
    /// reconciles the retry lineage's task ownership, spawns the detached
    /// pipeline worker, and returns the new run id as soon as the run is
    /// durable. Nothing about the resumed execution happens on the caller's
    /// thread, so run list / status / cancel stay answerable for its whole
    /// duration (F2026-07-122 defect 3) and the run is cancellable by pid like
    /// any other submitted run.
    ///
    /// [ORB-10709] Resuming creates another managed run, so it is a governed
    /// workflow operation and takes the same workspace-claim gate as
    /// [`Self::submit_ship_run`] — checked here, on the shared path, rather than
    /// in the adapters.
    pub fn submit_resume_run(
        &self,
        source_run_id: &str,
        actor: Option<&str>,
        claim_token: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.run.resume", claim_token)?;
        let plan = self.plan_job_run_resume(source_run_id)?;
        let job_id = plan.source.job_id.clone();
        self.submit_persisted_pipeline_run(PipelineSubmission {
            resume: Some(&plan),
            ..PipelineSubmission::catalog(&job_id, plan.input.clone(), actor)
        })
    }

    /// Submit a run for a catalog job id or a direct schemaVersion 2 job YAML
    /// path — the shared entry point behind `orbit run job` / `orbit job run`.
    ///
    /// [ORB-10801] Submission is one-shot: it validates the definition,
    /// persists the run, and hands it to a detached worker. A direct path is
    /// snapshotted next to the run before this returns, so the worker executes
    /// exactly the definition that was validated even if the source file is
    /// edited or deleted a moment later.
    pub fn submit_job_run(
        &self,
        job_ref: &str,
        input: Value,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let direct_path = Path::new(job_ref);
        if !direct_path.is_file() {
            let entry = self.show_job_catalog_entry(job_ref)?;
            if entry.kind() == orbit_types::workflow::JobKind::Subroutine {
                return Err(OrbitError::InvalidInput(format!(
                    "job '{}' declares `kind: subroutine` and cannot be run directly (asset: {}).",
                    entry.job_id,
                    entry.path.display()
                )));
            }
            return self.submit_pipeline_run(&entry.job_id, input, None, actor);
        }

        let (job_name, spec, yaml) = self.load_direct_job_definition(direct_path)?;
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            definition: SubmittedDefinition::Snapshot {
                spec: &spec,
                yaml: &yaml,
            },
            ..PipelineSubmission::catalog(&job_name, input.clone(), actor)
        });
        self.record_submission_audit(&job_name, &input, actor, &result)?;
        result
    }

    /// Read and fully validate a direct-path job definition in the submitting
    /// process, so a broken asset is refused before any run is persisted.
    fn load_direct_job_definition(
        &self,
        yaml_path: &Path,
    ) -> Result<(String, JobV2, String), OrbitError> {
        let yaml = std::fs::read_to_string(yaml_path).map_err(|error| {
            OrbitError::InvalidInput(format!("read {}: {error}", yaml_path.display()))
        })?;
        let asset = load_job_asset(&yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("load {}: {error}", yaml_path.display()))
        })?;
        validate_job_retired_sessions(&asset.spec, &yaml_path.display().to_string())
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        Ok((asset.name, asset.spec, yaml))
    }

    pub(crate) fn submit_automation_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        key: &str,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            action_key: Some(key),
            ..PipelineSubmission::catalog(job_name, input.clone(), Some("automation"))
        });
        self.record_submission_audit(job_name, &input, Some("automation"), &result)?;
        result
    }

    /// The only submission permitted to write [`OPERATION_ADMISSION_KEY`]:
    /// the grant-bound drain coordinator [ORB-11332]. Children inherit the
    /// snapshot at the parent-authorized admission path, never from input.
    pub(crate) fn submit_operation_bound_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            operation_bound: true,
            ..PipelineSubmission::catalog(job_name, input.clone(), actor)
        });
        self.record_submission_audit(job_name, &input, actor, &result)?;
        result
    }

    pub fn submit_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        priority: Option<&str>,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let result = self.submit_persisted_pipeline_run(PipelineSubmission::catalog(
            job_name,
            input.clone(),
            actor,
        ));

        self.record_pipeline_audit(
            "pipeline.invoke",
            result.as_ref().ok().map(|value| value.run_id.as_str()),
            actor,
            match &result {
                Ok(_) => AuditEventStatus::Success,
                Err(_) => AuditEventStatus::Failure,
            },
            json!({
                "actor": actor,
                "job_name": job_name,
                "priority": priority,
                "run_id": result.as_ref().ok().map(|value| value.run_id.clone()),
                "input_hash": input_hash(&input),
            }),
            result.as_ref().err().map(|error| error.to_string()),
        )?;

        result
    }

    /// Submit a v2 activity's child through the parent's durable admission
    /// boundary [ORB-11310].
    ///
    /// `Ok(None)` is the benign, idempotent result when the parent auto drain
    /// has already acknowledged an admissions stop. Direct/non-child callers
    /// continue to use [`Self::submit_pipeline_run`] and are unchanged.
    pub(crate) fn submit_child_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        priority: Option<&str>,
        actor: Option<&str>,
        admission: &ChildPipelineAdmission,
    ) -> Result<ChildSubmission, OrbitError> {
        let result = self.submit_persisted_pipeline_run_with_admission(
            PipelineSubmission::catalog(job_name, input.clone(), actor),
            Some(admission),
        );

        self.record_pipeline_audit(
            "pipeline.invoke",
            result.as_ref().ok().and_then(ChildSubmission::run_id),
            actor,
            match &result {
                Ok(_) => AuditEventStatus::Success,
                Err(_) => AuditEventStatus::Failure,
            },
            json!({
                "actor": actor,
                "job_name": job_name,
                "priority": priority,
                "parent_run_id": admission.parent_run_id,
                "outcome": match &result {
                    Ok(ChildSubmission::Skipped(reason)) => reason.as_str(),
                    _ => "submitted",
                },
                "run_id": result.as_ref().ok().and_then(ChildSubmission::run_id),
                "input_hash": input_hash(&input),
            }),
            result.as_ref().err().map(|error| error.to_string()),
        )?;

        result
    }

    /// Record the `pipeline.invoke` audit for a direct-path submission, which
    /// does not route through [`Self::submit_pipeline_run`].
    fn record_submission_audit(
        &self,
        job_name: &str,
        input: &Value,
        actor: Option<&str>,
        result: &Result<PipelineInvokeResult, OrbitError>,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            "pipeline.invoke",
            result.as_ref().ok().map(|value| value.run_id.as_str()),
            actor,
            match result {
                Ok(_) => AuditEventStatus::Success,
                Err(_) => AuditEventStatus::Failure,
            },
            json!({
                "actor": actor,
                "job_name": job_name,
                "priority": Option::<&str>::None,
                "run_id": result.as_ref().ok().map(|value| value.run_id.clone()),
                "input_hash": input_hash(input),
            }),
            result.as_ref().err().map(|error| error.to_string()),
        )
    }

    /// Persist a pipeline run and hand it to a detached worker.
    ///
    /// `resume` distinguishes the two submission shapes: `None` is a fresh
    /// attempt with a blank pipeline; `Some(plan)` links the new run to its
    /// source, seeds it with that source's checkpoints, and reconciles the
    /// lineage's task ownership before the worker can reach `worktree_setup`.
    fn submit_persisted_pipeline_run(
        &self,
        submission: PipelineSubmission<'_>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        match self.submit_persisted_pipeline_run_with_admission(submission, None)? {
            ChildSubmission::Submitted(result) => Ok(result),
            ChildSubmission::Skipped(reason) => Err(OrbitError::Execution(format!(
                "unconditional pipeline submission was refused as {reason}"
            ))),
        }
    }

    fn submit_persisted_pipeline_run_with_admission(
        &self,
        submission: PipelineSubmission<'_>,
        admission: Option<&ChildPipelineAdmission>,
    ) -> Result<ChildSubmission, OrbitError> {
        let PipelineSubmission {
            job_name,
            definition,
            input,
            resume,
            actor,
            action_key,
            trusted_host,
            operation_bound,
        } = submission;
        // [ORB-11354] The reserved admission key is writable by exactly one
        // caller. Refusing it here — on the single path every submission
        // surface funnels through — is what stops `orbit run job`, a resume,
        // an automation key, or a child dispatch from manufacturing an
        // unsandboxed run out of ordinary job input.
        if !trusted_host && run_input_declares_trusted_host(&input) {
            return Err(reserved_trusted_host_key_error(job_name));
        }
        // [ORB-11332] The operation-mode snapshot follows the same rule: the
        // grant-bound coordinator writes it, a resume carries its persisted
        // run input forward unchanged, and a parent-authorized child inherits
        // exactly its parent's snapshot. Any other input that names it is
        // refused rather than trusted.
        let (input, authority) = match admission {
            Some(admission) => {
                match child_admission_authority(self, &admission.parent_run_id, job_name, &input)? {
                    Some((snapshot, authority)) => {
                        let mut input = input;
                        inherit_child_admission(&mut input, &snapshot)?;
                        (input, Some(authority))
                    }
                    None => {
                        if run_input_declares_operation_admission(&input) {
                            return Err(reserved_operation_key_error(job_name));
                        }
                        (input, None)
                    }
                }
            }
            None => {
                if !operation_bound
                    && resume.is_none()
                    && run_input_declares_operation_admission(&input)
                {
                    return Err(reserved_operation_key_error(job_name));
                }
                (input, None)
            }
        };
        // [ORB-11333] The review admission follows the same discipline: a
        // child inherits its parent's snapshot, a grant-bound or ordinary
        // delivery submission captures the effective policy once, and
        // ordinary input naming the key is refused.
        let mut input = input;
        crate::application::review::install_review_admission(
            self,
            job_name,
            &mut input,
            admission.map(|admission| admission.parent_run_id.as_str()),
            resume.is_some(),
        )?;
        self.install_auto_crew_admission(
            job_name,
            &mut input,
            admission.map(|admission| admission.parent_run_id.as_str()),
            resume.is_some(),
            &mut super::crew_pools::random_crew_ticket,
        )?;
        let result = (|| {
            let spec = match &definition {
                SubmittedDefinition::Catalog => self.load_v2_job_asset_by_name(job_name)?.1,
                SubmittedDefinition::Snapshot { spec, .. } => (*spec).clone(),
            };
            if spec.state != JobScheduleState::Enabled {
                return Err(OrbitError::InvalidInput(format!(
                    "job '{job_name}' is disabled"
                )));
            }

            if job_name == "ci_failure_sweep_pipeline"
                && matches!(definition, SubmittedDefinition::Catalog)
            {
                self.resolve_ci_sweep_input(&spec, &mut input)?;
            }

            let submitted_at = Utc::now();
            let run = if let Some(admission) = admission {
                match self
                    .stores()
                    .jobs()
                    .admit_child_job_run(&ChildJobRunAdmissionParams {
                        parent_run_id: admission.parent_run_id.clone(),
                        parent_step_id: admission.parent_step_id.clone(),
                        job_id: job_name.to_string(),
                        action: admission.action.clone(),
                        blocking: admission.blocking,
                        attempt: 1,
                        scheduled_at: submitted_at,
                        input: Some(input.clone()),
                        authority: authority.clone(),
                    })? {
                    ChildJobRunAdmissionOutcome::Admitted(run) => *run,
                    ChildJobRunAdmissionOutcome::AdmissionsStopped => {
                        return Ok(ChildSubmission::Skipped("admissions_stopped".to_string()));
                    }
                    ChildJobRunAdmissionOutcome::Refused { reason } => {
                        return Ok(ChildSubmission::Skipped(reason));
                    }
                }
            } else if let Some(key) = action_key {
                self.stores()
                    .jobs()
                    .insert_automation_job_run(job_name, input.clone(), key)?
            } else {
                let run = self.stores().jobs().insert_job_run(
                    job_name,
                    resume.map_or(1, |plan| plan.attempt),
                    submitted_at,
                    Some(input.clone()),
                    resume.map(|plan| plan.source.run_id.clone()),
                )?;
                self.seed_v2_pipeline_run(&run, &input, resume)?;
                run
            };

            // Pin the definition before the worker can exist. A direct-path
            // submission must not depend on the source file surviving
            // unchanged until the detached worker gets around to reading it.
            if let SubmittedDefinition::Snapshot { yaml, .. } = &definition
                && let Err(error) = self.write_run_definition_snapshot(&run.run_id, yaml)
            {
                let _ =
                    self.finalize_pipeline_worker_startup_failure(&run, &error.to_string(), actor);
                return Err(error);
            }

            self.reconcile_stale_job_runs(Some(job_name))?;
            let active_runs = self
                .stores()
                .jobs()
                .list_pending_or_running_job_runs(job_name)?;
            let queued = !pipeline_run_is_runnable(&active_runs, &run.run_id, spec.max_active_runs);

            // A repeated automation admission resolves the original run. Only
            // pending runs need delivery; the existing Start CAS fences workers.
            if (action_key.is_none() || run.state == JobRunState::Pending)
                && let Err(error) = self.spawn_pipeline_worker(&run.run_id, actor)
            {
                let worker_log = pipeline_worker_log_path(&self.paths().logs_dir, &run.run_id)?;
                let message = format!(
                    "pipeline worker for run '{}' could not start from registered workspace '{}': \
                     {error}; worker log: '{}'",
                    run.run_id,
                    self.paths().repo_root.display(),
                    worker_log.display(),
                );
                let _ = self.finalize_pipeline_worker_startup_failure(&run, &message, actor);
                return Err(error);
            }
            Ok(ChildSubmission::Submitted(PipelineInvokeResult {
                run_id: run.run_id,
                job_name: job_name.to_string(),
                submitted_at: submitted_at.to_rfc3339(),
                queued,
            }))
        })();

        if let Some(plan) = resume {
            self.record_pipeline_audit(
                "pipeline.resume",
                result.as_ref().ok().and_then(ChildSubmission::run_id),
                actor,
                match &result {
                    Ok(_) => AuditEventStatus::Success,
                    Err(_) => AuditEventStatus::Failure,
                },
                json!({
                    "actor": actor,
                    "job_name": job_name,
                    "source_run_id": plan.source.run_id,
                    "attempt": plan.attempt,
                    "resumed_from_checkpoints": plan.resume_state.is_some(),
                    "checkpoint_batch_id": plan.checkpoint_batch_id,
                    "run_id": result.as_ref().ok().and_then(ChildSubmission::run_id),
                }),
                result.as_ref().err().map(|error| error.to_string()),
            )?;
        }

        result
    }

    /// Durably pin a submitted run's job definition next to the run record.
    fn write_run_definition_snapshot(&self, run_id: &str, yaml: &str) -> Result<(), OrbitError> {
        let dir = self.paths().job_runs_dir.clone();
        let path = run_definition_snapshot_path(&dir, run_id)?;
        atomic_write_text(&path, yaml).map_err(|error| {
            OrbitError::Io(format!(
                "write job run definition snapshot '{}': {error}",
                path.display()
            ))
        })
    }

    /// The definition a persisted run must execute: its own snapshot when the
    /// submission pinned one, otherwise the catalog asset named by the run.
    pub(crate) fn resolve_run_definition(
        &self,
        run: &JobRun,
    ) -> Result<(PathBuf, JobV2), OrbitError> {
        let snapshot = run_definition_snapshot_path(&self.paths().job_runs_dir, &run.run_id)?;
        if !snapshot.is_file() {
            return self.load_v2_job_asset_by_name(&run.job_id);
        }
        let yaml = std::fs::read_to_string(&snapshot).map_err(|error| {
            OrbitError::InvalidInput(format!("read {}: {error}", snapshot.display()))
        })?;
        let asset = load_job_asset(&yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("load {}: {error}", snapshot.display()))
        })?;
        Ok((snapshot, asset.spec))
    }

    pub fn wait_pipeline_runs(
        &self,
        run_ids: &[String],
        timeout_seconds: u64,
        poll_interval_seconds: u64,
        actor: Option<&str>,
    ) -> Result<PipelineWaitResult, OrbitError> {
        let started_payload = json!({
            "actor": actor,
            "run_ids": run_ids,
            "timeout_seconds": timeout_seconds,
        });
        self.record_pipeline_audit(
            "pipeline.wait.started",
            None,
            actor,
            AuditEventStatus::Success,
            started_payload,
            None,
        )?;

        let started_at = Instant::now();
        let timeout = Duration::from_secs(timeout_seconds);
        let poll = Duration::from_secs(poll_interval_seconds.max(PIPELINE_WAIT_MIN_POLL_SECONDS));

        loop {
            let snapshot = self.collect_pipeline_wait_entries(run_ids, false)?;
            if snapshot.iter().all(|entry| {
                matches!(
                    entry.status.as_str(),
                    "succeeded" | "failed" | "cancelled" | "interrupted"
                )
            }) {
                let result = PipelineWaitResult { results: snapshot };
                self.record_pipeline_wait_finished(actor, &result)?;
                return Ok(result);
            }

            if started_at.elapsed() >= timeout {
                let result = PipelineWaitResult {
                    results: self.collect_pipeline_wait_entries(run_ids, true)?,
                };
                self.record_pipeline_wait_finished(actor, &result)?;
                return Ok(result);
            }

            thread::sleep(poll);
        }
    }

    pub fn execute_pipeline_run_worker(&self, run_id: &str) -> Result<(), OrbitError> {
        self.preflight_pipeline_worker_store()?;

        // [ORB-10070] Claim the queued run for this worker process so orphan
        // reconciliation can tell a pending run whose worker is alive and
        // polling for its admission slot apart from one whose worker died
        // (crash, SIGKILL, host reboot). Best-effort: the run may already be
        // running/terminal, and a claim failure must never block execution.
        if let Err(error) = self
            .stores()
            .jobs()
            .claim_pending_job_run_owner(run_id, std::process::id())
        {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                error = %error,
                "pipeline worker could not claim its pending run; orphan \
                 detection falls back to the unclaimed-run grace window",
            );
        }
        loop {
            let run = self.show_job_run(run_id)?;
            match run.state {
                JobRunState::Pending => {}
                JobRunState::Running
                | JobRunState::Success
                | JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
                | JobRunState::Interrupted => return Ok(()),
                other => {
                    return Err(OrbitError::Execution(format!(
                        "pipeline worker cannot execute run '{}' from state '{}'",
                        run_id, other
                    )));
                }
            }

            let (yaml_path, spec) = self.resolve_run_definition(&run)?;
            if spec.state != JobScheduleState::Enabled {
                let _ = self.cancel_job_run(&run.run_id);
                return Err(OrbitError::InvalidInput(format!(
                    "job '{}' is disabled",
                    run.job_id
                )));
            }

            if let Err(error) = self.verify_routine_dispatch_workspace(&run) {
                self.record_routine_dispatch_workspace_mismatch(&run, &error);
                let _ = self.cancel_job_run(&run.run_id);
                return Err(error);
            }

            self.reconcile_stale_job_runs(Some(&run.job_id))?;
            let active_runs = self
                .stores()
                .jobs()
                .list_pending_or_running_job_runs(&run.job_id)?;
            if !pipeline_run_is_runnable(&active_runs, &run.run_id, spec.max_active_runs) {
                thread::sleep(Duration::from_secs(PIPELINE_WAIT_MIN_POLL_SECONDS));
                continue;
            }

            return self.execute_pipeline_run_now(&run, &yaml_path);
        }
    }

    /// [ORB-11998] A routine-dispatched run declares the `.orbit` directory of
    /// the workspace that owns it (see [`ROUTINE_DISPATCH_ORBIT_DIR_FIELD`]).
    /// Confirm this worker actually opened that same workspace before it runs
    /// any step. A mismatch — an ambient `ORBIT_ROOT`, an unregistered cwd, or
    /// any other workspace-routing failure — must fail the run visibly rather
    /// than silently execute (or vacuously succeed) against the wrong scope.
    /// A run with no declared field is not routine-dispatched and is
    /// unaffected.
    ///
    /// [ORB-12038] The refusal terminalizes the run as `cancelled`, not
    /// `failed`. It reuses [`Self::cancel_job_run`] unchanged — the same
    /// request/signal/completion audit trail, reservation release, and
    /// child-cascade settlement that every other cancellation gets — because
    /// nothing about that machinery is wrong here; only the missing
    /// diagnostic was. `failed` would read more accurately for a routing
    /// refusal than an operator action, but that relabeling is a wider
    /// contract change than this diagnostics fix and is deliberately left
    /// alone; see [`Self::record_routine_dispatch_workspace_mismatch`] for the
    /// diagnostic itself.
    fn verify_routine_dispatch_workspace(&self, run: &JobRun) -> Result<(), OrbitError> {
        let Some(declared) = run
            .input
            .as_ref()
            .and_then(|input| input.get(ROUTINE_DISPATCH_ORBIT_DIR_FIELD))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let declared_dir = Path::new(declared);
        let actual_dir = &self.paths().orbit_dir;
        if declared_dir == actual_dir.as_path() {
            return Ok(());
        }
        Err(OrbitError::WorkspaceError(format!(
            "run '{}' was dispatched for workspace '{}' but this worker resolved workspace '{}'; \
             refusing to execute against a mismatched workspace context",
            run.run_id,
            declared_dir.display(),
            actual_dir.display(),
        )))
    }

    /// [ORB-12038] Persist the guard's own message as a diagnostic step before
    /// [`Self::cancel_job_run`] terminalizes the run.
    ///
    /// The run is still `pending` here (`execute_pipeline_run_worker` has not
    /// reached `execute_pipeline_run_now`, so there is no `running` step to
    /// attach an error to), and cancellation itself records no error detail —
    /// it is written for an operator-requested stop, which carries no
    /// message. Without this, the guard's declared-vs-resolved diagnostic
    /// existed only in the worker process's own stderr and its
    /// `<run_id>.worker.log`, never on the run `orbit run show` displays.
    /// Best-effort: a failure to persist the diagnostic must not stop the
    /// run from being cancelled or the original error from propagating.
    fn record_routine_dispatch_workspace_mismatch(&self, run: &JobRun, error: &OrbitError) {
        let now = Utc::now();
        let _ = self.record_pipeline_diagnostic_step(
            run,
            run.scheduled_at,
            now,
            Some(ROUTINE_DISPATCH_WORKSPACE_MISMATCH_ERROR_CODE),
            &error.to_string(),
            JobRunState::Cancelled,
        );
    }

    /// Reopen the shared SQLite store before the worker claims a run.
    ///
    /// ADR-0287: another Orbit process may advance the host-global database
    /// while this runtime remains alive. Reopening here applies migrations
    /// supported by this binary or trips the downgrade guard before any agent
    /// work. Invocation persistence still reopens independently, but it can no
    /// longer be the first compatibility check after useful work completes.
    pub(crate) fn preflight_pipeline_worker_store(&self) -> Result<(), OrbitError> {
        self.ensure_persistence_ready()?;
        Ok(())
    }

    pub fn normalize_pipeline_wait_timeout(raw: Option<u64>) -> Result<u64, OrbitError> {
        let timeout_seconds = raw.unwrap_or(PIPELINE_WAIT_DEFAULT_TIMEOUT_SECONDS);
        if timeout_seconds > PIPELINE_WAIT_MAX_TIMEOUT_SECONDS {
            return Err(OrbitError::InvalidInput(format!(
                "`timeout_seconds` must be <= {PIPELINE_WAIT_MAX_TIMEOUT_SECONDS}"
            )));
        }
        Ok(timeout_seconds)
    }

    pub fn normalize_pipeline_wait_poll_interval(raw: Option<u64>) -> u64 {
        raw.unwrap_or(PIPELINE_WAIT_DEFAULT_POLL_SECONDS)
            .max(PIPELINE_WAIT_MIN_POLL_SECONDS)
    }

    /// [ORB-10965] Record a duplicate Start that was dropped without a second
    /// execution.
    ///
    /// Delivery is at-least-once, so losing the Start race is an expected
    /// outcome, not a fault: it is logged and audited as its own event and the
    /// worker returns successfully, leaving the run's real state to the worker
    /// that does own it.
    fn record_deduplicated_start(&self, run: &JobRun, reason: &str) -> Result<(), OrbitError> {
        tracing::info!(
            target: "orbit.core.job_run",
            run_id = %run.run_id,
            job_id = %run.job_id,
            attempt = run.attempt,
            reason,
            "duplicate job run start delivery deduplicated; the incumbent \
             owner keeps execution authority",
        );
        self.record_event(OrbitEvent::JobRunStartDeduplicated {
            job_id: run.job_id.clone(),
            run_id: run.run_id.clone(),
            attempt: run.attempt,
            reason: reason.to_string(),
        })
    }

    fn execute_pipeline_run_now(&self, run: &JobRun, yaml_path: &Path) -> Result<(), OrbitError> {
        let started_at = Utc::now();
        // [ORB-10965] The state read in `execute_pipeline_run_worker` and this
        // Start are separate transactions, so a second worker handed the same
        // queued run can arrive here having also seen `pending`. The store
        // arbitrates atomically; whoever does not win execution authority
        // yields here, before any agent work.
        let outcome = match self.stores().jobs().mark_job_run_running(
            &run.run_id,
            started_at,
            std::process::id(),
        ) {
            Ok(outcome) => outcome,
            Err(OrbitError::JobRunStartConflict(diagnostic)) => {
                return self.record_deduplicated_start(run, &diagnostic);
            }
            Err(error) => return Err(error),
        };
        match outcome {
            JobRunStartOutcome::Started => {}
            JobRunStartOutcome::AlreadyStarted => {
                return self.record_deduplicated_start(
                    run,
                    "this worker process had already started the run",
                );
            }
            JobRunStartOutcome::NotFound => return Ok(()),
        }
        let input = run
            .input
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        // Once Start succeeds, every later error belongs to this run. Keep the
        // whole setup path inside the outcome finalized below so crew
        // validation, event persistence, and resume-state reads cannot escape
        // with a durable `running` projection.
        let outcome = (|| {
            self.record_run_crew_from_input(&run.run_id, &input)?;

            self.record_event(OrbitEvent::JobRunStarted {
                job_id: run.job_id.clone(),
                run_id: run.run_id.clone(),
                attempt: run.attempt,
            })?;

            // [ORB-10470] The run's own persisted checkpoints are the resume
            // cursor. A run seeded by `submit_resume_run` starts at the
            // source's first non-successful step; a run whose previous worker
            // died after checkpointing continues from where that worker
            // stopped. Reusing a checkpoint is therefore idempotent — the
            // successful steps are skipped, never re-dispatched.
            let resume = self.read_run_state(&run.run_id)?.filter(|state| {
                state
                    .step_states
                    .values()
                    .any(|step_state| *step_state == JobRunState::Success)
            });
            self.run_job_v2_from_yaml_with_run_id_and_resume(
                yaml_path,
                input.clone(),
                Some(run.run_id.clone()),
                run.retry_source_run_id.clone(),
                resume.as_ref(),
            )
        })();
        let finished_at = Utc::now();
        self.finalize_v2_pipeline_run(
            run,
            &input,
            started_at,
            finished_at,
            outcome.as_ref(),
            V2RunFinalizationOptions::DETACHED_WORKER,
        )?;
        outcome.map(|_| ())
    }

    pub(crate) fn record_pipeline_failure_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        message: &str,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_diagnostic_step(
            run,
            started_at,
            finished_at,
            None,
            message,
            JobRunState::Failed,
        )
    }

    /// [ORB-10002] Record a terminal diagnostic step with an explicit state
    /// (`failed` for job errors, `interrupted` for orphan reconciliation).
    pub(crate) fn record_pipeline_diagnostic_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        let current = self
            .get_job_run_backend(&run.run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run.run_id.clone()))?;
        let already_has_error = current
            .steps
            .iter()
            .any(|step| step.error_code.is_some() || step.error_message.is_some());
        if already_has_error {
            return Ok(());
        }

        let step_index = current
            .steps
            .iter()
            .map(|step| step.step_index)
            .max()
            .map(|index| index.saturating_add(1) as usize)
            .unwrap_or(0);
        let duration_ms = Some(
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        let params = JobRunStepParams {
            step_index,
            target_type: JobTargetType::Job,
            target_id: run.job_id.clone(),
            started_at,
            finished_at,
            duration_ms,
            exit_code: None,
            agent_response_json: None,
            state,
            error_code: error_code.map(str::to_string),
            error_message: Some(message.to_string()),
        };
        let _ = self
            .stores()
            .jobs()
            .complete_job_run_step(&run.run_id, &params)?;
        Ok(())
    }

    /// [ORB-12038] Read a run's own `<run_id>.worker.log`, for a caller (`orbit
    /// run logs`) that found no audited CLI-invocation blobs to show. A run
    /// that fails before any step runs — a routine-dispatch workspace
    /// mismatch, a worker that could not start at all — has no step-scoped
    /// output to audit; the worker log is where that process wrote its own
    /// stderr, and it is the only place the cause exists.
    ///
    /// `Ok(None)` means no such file exists (the ordinary case for a run that
    /// reached step execution). `Some` with `content: None` means the file
    /// exists but its content could not be recovered (unreadable or empty);
    /// the caller still has the path to name where to look by hand.
    pub fn read_pipeline_worker_log(
        &self,
        run_id: &str,
    ) -> Result<Option<PipelineWorkerLogSnapshot>, OrbitError> {
        let path = pipeline_worker_log_path(&self.paths().logs_dir, run_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let mut file = File::open(&path).map_err(|error| {
            OrbitError::Io(format!(
                "open pipeline worker log '{}': {error}",
                path.display()
            ))
        })?;
        let content = read_pipeline_worker_log_tail(&mut file);
        Ok(Some(PipelineWorkerLogSnapshot { path, content }))
    }

    fn collect_pipeline_wait_entries(
        &self,
        run_ids: &[String],
        timeout_incomplete: bool,
    ) -> Result<Vec<PipelineWaitEntry>, OrbitError> {
        run_ids
            .iter()
            .map(|run_id| {
                let run = match self.show_job_run(run_id) {
                    Ok(run) => run,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::JobRun,
                        ..
                    }) => {
                        return Ok(PipelineWaitEntry {
                            run_id: run_id.clone(),
                            status: "failed".to_string(),
                            finished_at: None,
                            pipeline: None,
                            error: Some("unknown run".to_string()),
                        });
                    }
                    Err(error) => return Err(error),
                };

                let terminal = match run.state {
                    JobRunState::Success => Some("succeeded"),
                    JobRunState::Failed => Some("failed"),
                    JobRunState::Cancelled => Some("cancelled"),
                    JobRunState::Interrupted => Some("interrupted"),
                    _ => None,
                };
                let status = match (terminal, timeout_incomplete) {
                    (Some(status), _) => status.to_string(),
                    (None, true) => "timeout".to_string(),
                    (None, false) => run.state.to_string(),
                };
                let pipeline = if matches!(status.as_str(), "timeout") {
                    None
                } else {
                    self.read_run_state(run_id)?.map(|state| state.pipeline)
                };
                let error = if matches!(status.as_str(), "failed" | "cancelled" | "interrupted") {
                    let (code, message) = run
                        .steps
                        .iter()
                        .rev()
                        .find(|step| step.error_code.is_some() || step.error_message.is_some())
                        .map(|step| (step.error_code.clone(), step.error_message.clone()))
                        .unwrap_or((None, None));
                    match (code, message) {
                        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
                        (Some(code), None) => Some(code),
                        (None, Some(message)) => Some(message),
                        (None, None) => None,
                    }
                } else {
                    None
                };
                Ok(PipelineWaitEntry {
                    run_id: run_id.clone(),
                    status,
                    finished_at: run.finished_at.map(|value| value.to_rfc3339()),
                    pipeline,
                    error,
                })
            })
            .collect()
    }

    fn spawn_pipeline_worker(&self, run_id: &str, actor: Option<&str>) -> Result<(), OrbitError> {
        let mut command = self.pipeline_worker_command(run_id)?;
        let worker_log =
            configure_pipeline_worker_stdio(&mut command, &self.paths().logs_dir, run_id)?;
        self.spawn_pipeline_worker_process(run_id, actor, command, worker_log)
            .map(|_| ())
    }

    /// The program a detached worker runs: this same `orbit` binary, re-entered
    /// at the hidden worker subcommand. Workspace context is discovered by cwd;
    /// an explicit parent `--root` is forwarded so the child opens the same
    /// global store the parent used to persist the run [ORB-10821].
    fn pipeline_worker_command(&self, run_id: &str) -> Result<Command, OrbitError> {
        let paths = self.paths();
        #[cfg(test)]
        {
            worker_command_override::command(&paths.repo_root, run_id).ok_or_else(|| {
                OrbitError::Execution(
                    "test pipeline worker requires an explicit worker command override".to_string(),
                )
            })
        }

        #[cfg(not(test))]
        {
            let current_exe = std::env::current_exe().map_err(|error| {
                OrbitError::Execution(format!("resolve current orbit executable: {error}"))
            })?;
            let mut command = Command::new(resolve_pipeline_worker_executable(current_exe));
            configure_pipeline_worker_command(
                &mut command,
                &paths.repo_root,
                run_id,
                pipeline_worker_root_override(paths),
            );
            Ok(command)
        }
    }

    pub(crate) fn spawn_pipeline_worker_process(
        &self,
        run_id: &str,
        actor: Option<&str>,
        mut command: Command,
        worker_log: PipelineWorkerLog,
    ) -> Result<u32, OrbitError> {
        let PipelineWorkerLog {
            path: worker_log,
            reader: worker_log_reader,
        } = worker_log;

        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        // Start the observer before the process so every successfully spawned
        // worker has a parent-side path that can terminalize a pre-claim exit.
        // Cwd still carries the registered workspace. `--root` is forwarded
        // only when the parent itself was pinned (see
        // `pipeline_worker_root_override`); passing the workspace `.orbit`
        // path here used to pin both roots and disconnect the worker from
        // `$HOME/.orbit/orbit.db`.
        let (sender, receiver) = mpsc::sync_channel::<Child>(1);
        let runtime = self.clone();
        let run_id_for_observer = run_id.to_string();
        let actor_for_observer = actor.map(ToOwned::to_owned);
        let workspace_for_observer = self.paths().repo_root.clone();
        let worker_log_for_observer = worker_log.clone();
        thread::Builder::new()
            .name(format!("pipeline-start-{run_id}"))
            .spawn(move || {
                let Ok(child) = receiver.recv() else {
                    return;
                };
                if let Err(error) = runtime.monitor_pipeline_worker_startup(
                    &run_id_for_observer,
                    child,
                    &workspace_for_observer,
                    &worker_log_for_observer,
                    worker_log_reader,
                    actor_for_observer.as_deref(),
                ) {
                    tracing::error!(
                        target: "orbit.core.job_run",
                        run_id = run_id_for_observer,
                        error = %error,
                        "failed to observe pipeline worker startup",
                    );
                }
            })
            .map_err(|error| {
                OrbitError::Execution(format!("spawn pipeline worker observer: {error}"))
            })?;

        let child = command
            .spawn()
            .map_err(|error| OrbitError::Execution(format!("spawn pipeline worker: {error}")))?;
        let child_pid = child.id();
        sender.send(child).map_err(|error| {
            OrbitError::Execution(format!("hand pipeline worker to startup observer: {error}"))
        })?;
        Ok(child_pid)
    }

    pub(crate) fn monitor_pipeline_worker_startup(
        &self,
        run_id: &str,
        mut child: Child,
        workspace: &Path,
        worker_log: &Path,
        mut worker_log_reader: File,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let child_pid = child.id();
        let mut claimed = false;
        loop {
            #[cfg(test)]
            worker_observer_read_counter::record(self, run_id);
            let run = self
                .get_job_run_backend(run_id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
            if run.pid == Some(child_pid) && !claimed {
                let _ = self.record_pipeline_audit(
                    "pipeline.worker.claimed",
                    Some(run_id),
                    actor,
                    AuditEventStatus::Success,
                    json!({
                        "run_id": run_id,
                        "worker_pid": child_pid,
                        "owner_pid": child_pid,
                        "workspace": workspace,
                        "worker_log": worker_log,
                        "state": run.state.to_string(),
                    }),
                    None,
                );
                claimed = true;
            }

            // A persisted owner or non-pending state settles the only startup
            // question this observer owns. Waiting for the child avoids a
            // full run/step SQLite read every 25ms throughout normal work.
            let status = if run.pid.is_some() || run.state != JobRunState::Pending {
                Some(child.wait().map_err(|error| {
                    OrbitError::Execution(format!(
                        "wait for pipeline worker process for run '{run_id}': {error}"
                    ))
                })?)
            } else {
                child.try_wait().map_err(|error| {
                    OrbitError::Execution(format!(
                        "observe pipeline worker process for run '{run_id}': {error}"
                    ))
                })?
            };

            if let Some(status) = status {
                // The worker may have changed the run after the last startup
                // observation. Exit handling must use fresh state so duplicate
                // ownership, cancellation, and terminal outcomes stay
                // authoritative.
                #[cfg(test)]
                worker_observer_read_counter::record(self, run_id);
                let run = self.get_job_run_backend(run_id)?.ok_or_else(|| {
                    OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string())
                })?;
                let output = read_pipeline_worker_log_tail(&mut worker_log_reader);
                let output_detail = output
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .map(|value| format!("; worker output:\n{value}"))
                    .unwrap_or_default();
                // [ORB-11116] A second worker can lose the atomic Start race
                // and exit successfully while the incumbent's PID remains on
                // the run. Only this observer's exact child PID establishes
                // ownership; another non-null PID is a benign duplicate
                // delivery, not evidence that this child abandoned the run.
                if let Some(owner_pid) = run.pid.filter(|owner_pid| *owner_pid != child_pid) {
                    tracing::info!(
                        target: "orbit.core.job_run",
                        run_id,
                        worker_pid = child_pid,
                        owner_pid,
                        exit_status = %status,
                        "duplicate pipeline worker exited without owning the persisted run",
                    );
                    let _ = self.record_pipeline_audit(
                        "pipeline.worker.duplicate",
                        Some(run_id),
                        actor,
                        AuditEventStatus::Success,
                        json!({
                            "run_id": run_id,
                            "worker_pid": child_pid,
                            "owner_pid": owner_pid,
                            "workspace": workspace,
                            "worker_log": worker_log,
                            "state": run.state.to_string(),
                            "exit_status": status.to_string(),
                        }),
                        None,
                    );
                    return Ok(());
                }
                #[cfg(unix)]
                if let Some(signal) = status
                    .signal()
                    .filter(|signal| matches!(*signal, libc::SIGTERM | libc::SIGKILL))
                    && self.record_pipeline_worker_cancellation_exit(
                        &run,
                        signal,
                        &status.to_string(),
                        actor,
                    )?
                {
                    // The cancelling caller owns terminalization after it has
                    // verified both the recorded leader and process group are
                    // gone. Reaping the worker proves only the leader exited;
                    // finalizing here could release reservations while a
                    // run-owned child remains alive.
                    return Ok(());
                }
                if run.state.is_terminal() {
                    return Ok(());
                }
                let ownership = if run.pid == Some(child_pid) {
                    "after claiming"
                } else {
                    "before claiming"
                };
                let message = format!(
                    "pipeline worker for run '{run_id}' exited with status {status} {ownership} \
                     the persisted run from registered workspace '{}'; worker log: \
                     '{}'{output_detail}; verify workspace registration, worker root discovery, \
                     and action availability",
                    workspace.display(),
                    worker_log.display(),
                );
                self.finalize_pipeline_worker_exit_failure(&run, &message, actor)?;
                return Ok(());
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Record a TERM/KILL worker exit that belongs to an outstanding
    /// cancellation request, without terminalizing the run. The signalling
    /// caller performs the authoritative liveness verification and then
    /// finalizes `cancelled`; this observer only preserves the completion
    /// cause and suppresses the misleading generic worker-failure path.
    #[cfg(unix)]
    pub(crate) fn record_pipeline_worker_cancellation_exit(
        &self,
        run: &JobRun,
        signal: i32,
        exit_status: &str,
        actor: Option<&str>,
    ) -> Result<bool, OrbitError> {
        let Some(request_id) = self.active_job_run_cancellation_request(&run.run_id)? else {
            return Ok(false);
        };
        self.record_pipeline_audit(
            CANCELLATION_WORKER_EXIT_AUDIT,
            Some(&run.run_id),
            actor,
            AuditEventStatus::Success,
            json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "owner_pid": run.pid,
                "signal": signal,
                "signal_name": worker_cancellation_signal_name(signal),
                "exit_status": exit_status,
                "observed_at": Utc::now().to_rfc3339(),
            }),
            None,
        )?;
        Ok(true)
    }

    /// Terminalize a worker process that exited while it still owned a
    /// non-terminal run. `try_wait` has already reaped the process when this is
    /// called. Pending exits are interrupted startup; a worker that reached
    /// running failed its claimed execution.
    fn finalize_pipeline_worker_exit_failure(
        &self,
        run: &JobRun,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let current = self
            .get_job_run_backend(&run.run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run.run_id.clone()))?;
        let (state, started_at, audit_name) = match current.state {
            JobRunState::Pending => (
                JobRunState::Interrupted,
                current.scheduled_at,
                "pipeline.worker.startup",
            ),
            JobRunState::Running => (
                JobRunState::Failed,
                current.started_at.unwrap_or(current.scheduled_at),
                "pipeline.worker.exit",
            ),
            _ => return Ok(()),
        };
        let finished_at = Utc::now();
        self.record_pipeline_diagnostic_step(
            &current,
            started_at,
            finished_at,
            None,
            message,
            state,
        )?;
        let changed = self.finalize_job_run_with_reservation_cleanup(
            &current.run_id,
            state,
            finished_at,
            None,
            TaskReservationReleaseReason::RunTerminal,
        )?;
        if changed {
            self.record_event(OrbitEvent::JobRunCompleted {
                job_id: current.job_id.clone(),
                run_id: current.run_id.clone(),
                state: state.to_string(),
            })?;
        }
        let worker_log = pipeline_worker_log_path(&self.paths().logs_dir, &current.run_id)?;
        self.record_pipeline_audit(
            audit_name,
            Some(&current.run_id),
            actor,
            AuditEventStatus::Failure,
            json!({
                "run_id": current.run_id,
                "workspace": self.paths().repo_root,
                "worker_log": worker_log,
            }),
            Some(message.to_string()),
        )
    }

    fn finalize_pipeline_worker_startup_failure(
        &self,
        run: &JobRun,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let current = self.show_job_run(&run.run_id)?;
        if current.state != JobRunState::Pending || current.pid.is_some() {
            return Ok(());
        }

        let finished_at = Utc::now();
        // Persist the diagnostic step before terminalizing the run: an observer
        // polling for a terminal state must never be able to see one without its
        // startup diagnostic already durable.
        self.record_pipeline_diagnostic_step(
            run,
            run.scheduled_at,
            finished_at,
            None,
            message,
            JobRunState::Interrupted,
        )?;
        let changed = self.finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Interrupted,
            finished_at,
            None,
            TaskReservationReleaseReason::RunTerminal,
        )?;
        if changed {
            self.record_event(OrbitEvent::JobRunCompleted {
                job_id: run.job_id.clone(),
                run_id: run.run_id.clone(),
                state: JobRunState::Interrupted.to_string(),
            })?;
        }
        let worker_log = pipeline_worker_log_path(&self.paths().logs_dir, &run.run_id)?;
        self.record_pipeline_audit(
            "pipeline.worker.startup",
            Some(&run.run_id),
            actor,
            AuditEventStatus::Failure,
            json!({
                "run_id": run.run_id,
                "workspace": self.paths().repo_root,
                "worker_log": worker_log,
            }),
            Some(message.to_string()),
        )
    }

    fn record_pipeline_wait_finished(
        &self,
        actor: Option<&str>,
        result: &PipelineWaitResult,
    ) -> Result<(), OrbitError> {
        let mut succeeded = 0usize;
        let mut failed = 0usize;
        let mut cancelled = 0usize;
        let mut timeout = 0usize;
        for entry in &result.results {
            match entry.status.as_str() {
                "succeeded" => succeeded += 1,
                "failed" => failed += 1,
                "cancelled" => cancelled += 1,
                "timeout" => timeout += 1,
                _ => {}
            }
        }

        self.record_pipeline_audit(
            "pipeline.wait.finished",
            None,
            actor,
            AuditEventStatus::Success,
            json!({
                "actor": actor,
                "results_summary": {
                    "succeeded": succeeded,
                    "failed": failed,
                    "cancelled": cancelled,
                    "timeout": timeout,
                },
            }),
            None,
        )
    }

    pub(crate) fn record_pipeline_audit(
        &self,
        tool_name: &str,
        target_id: Option<&str>,
        actor: Option<&str>,
        status: AuditEventStatus,
        arguments: Value,
        error_message: Option<String>,
    ) -> Result<(), OrbitError> {
        let arguments_json = serde_json::to_string(&arguments).map_err(|error| {
            OrbitError::Store(format!("serialize pipeline audit args: {error}"))
        })?;
        let execution_id = audit_execution_id("exec");
        self.record_audit_event(&AuditEventInsertParams {
            execution_id,
            command: "tool".to_string(),
            subcommand: Some("run".to_string()),
            tool_name: Some(tool_name.to_string()),
            target_type: Some("job_run".to_string()),
            target_id: target_id.map(ToOwned::to_owned),
            role: "admin".to_string(),
            status,
            exit_code: if status == AuditEventStatus::Success {
                0
            } else {
                1
            },
            duration_ms: 0,
            working_directory: self.paths().repo_root.display().to_string(),
            arguments_json: Some(arguments_json),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message,
            host: actor.map(ToOwned::to_owned),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: target_id.map(ToOwned::to_owned),
            activity_id: None,
            step_index: None,
        })
    }
}

#[cfg(unix)]
fn worker_cancellation_signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGTERM => "SIGTERM",
        libc::SIGKILL => "SIGKILL",
        _ => "unknown",
    }
}

/// Return a stable path suitable for launching a fresh worker process.
///
/// Linux exposes a process whose executable inode was unlinked as
/// `/installed/path (deleted)`. That pseudo-path cannot be executed, but after
/// an atomic upgrade the original installed path names the replacement binary.
/// Preserve ordinary paths, including real filenames ending in ` (deleted)`.
pub(crate) fn resolve_pipeline_worker_executable(current_exe: PathBuf) -> PathBuf {
    // L-0084: deleted Linux executable paths must resolve through the installed replacement.
    #[cfg(target_os = "linux")]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let current_path_is_missing = matches!(
            std::fs::metadata(&current_exe),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        if current_path_is_missing
            && let Some(installed_path) = current_exe
                .as_os_str()
                .as_bytes()
                .strip_suffix(b" (deleted)")
        {
            return PathBuf::from(OsString::from_vec(installed_path.to_vec()));
        }
    }

    current_exe
}

/// Forward `--root` only when the parent runtime is pinned to one directory
/// (`global_dir == orbit_dir`). That is the `--root` flag's contract: it pins
/// both the workspace and the global store. The default split-root layout
/// (`$HOME/.orbit` vs workspace `.orbit`) must keep this `None` — an explicit
/// `--root` would pin *both* roots and disconnect the worker from the global
/// registry database that contains the persisted run.
pub(crate) fn pipeline_worker_root_override(paths: &WorkspacePaths) -> Option<&Path> {
    (paths.global_dir == paths.orbit_dir).then_some(paths.global_dir.as_path())
}

pub(crate) fn configure_pipeline_worker_command(
    command: &mut Command,
    workspace: &Path,
    run_id: &str,
    root_override: Option<&Path>,
) {
    // [ORB-11998] `resolve_roots` prefers an `ORBIT_ROOT` env value over cwd
    // walk-up, so an inherited value — from the sweep clock's own service
    // environment, an operator's shell, or any other ambient source — would
    // silently redirect this worker to a different registered workspace than
    // the one `current_dir` below pins it to. Every worker gets an explicit
    // workspace identity, either via `--root` (pinned parent) or cwd (default
    // split-root layout), so `ORBIT_ROOT` must never be left to compete with
    // either.
    command.env_remove("ORBIT_ROOT");
    if let Some(root) = root_override {
        // `--root` pins both stores.
        command.arg("--root").arg(root);
    }
    command
        .arg("job")
        .arg("run-pipeline-worker")
        .arg(run_id)
        .current_dir(workspace)
        .stdin(Stdio::null());
}

/// Give a detached worker its own coverage dump path when the parent inherited
/// `LLVM_PROFILE_FILE` (cargo-llvm-cov / instrumented CI).
///
/// The child is the same instrumented `orbit` binary. Sharing the parent's
/// profile file — or following a relative `LLVM_PROFILE_FILE` after cwd is
/// moved to the registered workspace — can stall or abort in CRT init, before
/// `main`, so the run never gets a PID and the worker log stays empty.
pub(crate) fn pipeline_worker_profile_file(
    logs_dir: &Path,
    run_id: &str,
    inherited: Option<&OsStr>,
) -> Result<Option<PathBuf>, OrbitError> {
    let Some(_) = inherited.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    Ok(Some(
        logs_dir.join(pipeline_worker_file_name(run_id, ".%p.profraw")?),
    ))
}

/// Where a submitted run's pinned job definition lives.
pub(crate) fn run_definition_snapshot_path(
    job_runs_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    Ok(job_runs_dir.join(pipeline_worker_file_name(run_id, ".job.yaml")?))
}

pub(crate) fn pipeline_worker_log_path(
    logs_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    Ok(logs_dir.join(pipeline_worker_file_name(run_id, ".worker.log")?))
}

/// Turn a persisted run ID into a filename only after rejecting path syntax.
///
/// Run IDs normally come from the store, but worker entry points also accept
/// an ID from a process argument. Keeping this check at the shared filename
/// boundary prevents either source from steering pipeline artifacts outside
/// their owning directory.
fn pipeline_worker_file_name(run_id: &str, suffix: &str) -> Result<String, OrbitError> {
    let safe = !run_id.is_empty()
        && run_id != "."
        && run_id != ".."
        && !run_id.contains(['/', '\\'])
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !safe {
        return Err(OrbitError::InvalidInput(format!(
            "job run id must be a safe filename stem: {run_id}"
        )));
    }

    Ok(format!("{run_id}{suffix}"))
}

/// Resolve the worker-log directory before using it for any file operation.
///
/// Containment is the nearest existing parent, canonicalized, plus the final
/// component. Ancestor symlinks are followed rather than rejected so ordinary
/// layouts (a symlinked `/tmp`, `$HOME`, or project root) can still spawn a
/// worker. A missing parent is rebuilt from that canonical ancestor so the
/// caller can create intermediate directories. The final component itself
/// must not be a symlink or a non-directory; traversal syntax fails closed.
#[cfg(not(unix))]
fn validated_pipeline_worker_log_directory(path: &Path) -> Result<PathBuf, OrbitError> {
    let validated_path = validate_pipeline_worker_log_directory_input(path)?;
    let parent = validated_path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no parent: {}",
            path.display()
        ))
    })?;
    let file_name = validated_path.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no final component: {}",
            path.display()
        ))
    })?;
    let canonical_parent = canonical_pipeline_worker_log_parent(parent)?;
    let canonical_path = canonical_parent.join(file_name);
    if !canonical_path.starts_with(&canonical_parent) {
        return Err(OrbitError::InvalidInput(format!(
            "pipeline worker log directory must not contain traversal components: {}",
            path.display()
        )));
    }

    match std::fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(OrbitError::InvalidInput(format!(
                "pipeline worker log directory must not be a symlink: {}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(OrbitError::InvalidInput(format!(
                "pipeline worker log path is not a directory: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect pipeline worker log directory '{}': {error}",
                path.display()
            )));
        }
    }

    Ok(canonical_path)
}

#[cfg(unix)]
struct PipelineWorkerLogDirectory {
    path: PathBuf,
    directory: File,
}

/// Open the validated authority and create the log directory beneath its fd.
///
/// The trusted authority is the inode of the nearest existing ancestor after
/// resolving aliases which already existed when setup began. That preserves
/// supported aliases such as a symlinked `/tmp`, home, or project root. The
/// inode is checked while opening it, then every missing suffix component is
/// created and opened relative to the held descriptor with symlink following
/// disabled. Renames after that point cannot redirect creation, open, or chmod
/// to another filesystem object.
#[cfg(unix)]
fn prepare_pipeline_worker_log_directory(
    path: &Path,
) -> Result<PipelineWorkerLogDirectory, OrbitError> {
    let validated_path = validate_pipeline_worker_log_directory_input(path)?;
    let parent = validated_path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no parent: {}",
            path.display()
        ))
    })?;
    let final_name = validated_path.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no final component: {}",
            path.display()
        ))
    })?;
    let (authority_path, missing, expected_authority) =
        canonical_pipeline_worker_log_parent_components(parent)?;

    #[cfg(all(test, unix))]
    pipeline_worker_log_test_hook::run(
        pipeline_worker_log_test_hook::Phase::AuthorityValidated,
        &authority_path,
    );

    let mut directory = open_pipeline_worker_directory(None, &authority_path)?;
    let opened_authority = directory.metadata().map_err(|error| {
        OrbitError::Io(format!(
            "inspect opened pipeline worker log authority '{}': {error}",
            authority_path.display()
        ))
    })?;
    if expected_authority.dev() != opened_authority.dev()
        || expected_authority.ino() != opened_authority.ino()
    {
        return Err(OrbitError::InvalidInput(format!(
            "pipeline worker log authority changed while it was opened: {}",
            authority_path.display()
        )));
    }

    let mut resolved_path = authority_path;
    for component in missing
        .iter()
        .map(OsString::as_os_str)
        .chain(std::iter::once(final_name))
    {
        resolved_path.push(component);
        create_pipeline_worker_directory_at(&directory, component, &resolved_path)?;
        directory = open_pipeline_worker_directory(Some(&directory), Path::new(component))?;
    }

    Ok(PipelineWorkerLogDirectory {
        path: resolved_path,
        directory,
    })
}

#[cfg(unix)]
fn canonical_pipeline_worker_log_parent_components(
    parent: &Path,
) -> Result<(PathBuf, Vec<OsString>, std::fs::Metadata), OrbitError> {
    let mut existing = parent.to_path_buf();
    let mut missing = Vec::<OsString>::new();
    loop {
        match std::fs::metadata(&existing) {
            Ok(metadata) => {
                let canonical = existing.canonicalize().map_err(|error| {
                    OrbitError::Io(format!(
                        "resolve pipeline worker log authority '{}': {error}",
                        existing.display()
                    ))
                })?;
                missing.reverse();
                return Ok((canonical, missing, metadata));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                };
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                }
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect pipeline worker log directory '{}': {error}",
                    existing.display()
                )));
            }
        }
    }
}

#[cfg(unix)]
fn pipeline_worker_component_c_string(component: &OsStr) -> Result<std::ffi::CString, OrbitError> {
    std::ffi::CString::new(component.as_bytes()).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log path contains an invalid null byte: {}",
            component.to_string_lossy()
        ))
    })
}

#[cfg(unix)]
fn open_pipeline_worker_directory(parent: Option<&File>, path: &Path) -> Result<File, OrbitError> {
    let path_c = pipeline_worker_component_c_string(path.as_os_str())?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = match parent {
        Some(parent) => unsafe { libc::openat(parent.as_raw_fd(), path_c.as_ptr(), flags) },
        None => unsafe { libc::open(path_c.as_ptr(), flags) },
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR)
        {
            return Err(OrbitError::InvalidInput(format!(
                "pipeline worker log directory must not be a symlink or non-directory: {}",
                path.display()
            )));
        }
        return Err(OrbitError::Io(format!(
            "open pipeline worker log directory '{}': {error}",
            path.display(),
        )));
    }

    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn create_pipeline_worker_directory_at(
    parent: &File,
    component: &OsStr,
    display_path: &Path,
) -> Result<(), OrbitError> {
    let component_c = pipeline_worker_component_c_string(component)?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), component_c.as_ptr(), 0o700) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(OrbitError::Io(format!(
                "create pipeline worker log directory '{}': {error}",
                display_path.display()
            )));
        }
    }

    Ok(())
}

#[cfg(unix)]
fn open_pipeline_worker_log_at(
    directory: &File,
    file_name: &OsStr,
    display_path: &Path,
) -> Result<File, OrbitError> {
    let file_name_c = pipeline_worker_component_c_string(file_name)?;
    let flags = libc::O_CREAT | libc::O_APPEND | libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::openat(directory.as_raw_fd(), file_name_c.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(OrbitError::Io(format!(
            "open pipeline worker log '{}': {}",
            display_path.display(),
            std::io::Error::last_os_error()
        )));
    }

    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(all(test, unix))]
pub(crate) mod pipeline_worker_log_test_hook {
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Phase {
        AuthorityValidated,
        DirectoryReady,
        BeforeLogOpen,
    }

    type Hook = Box<dyn FnOnce(&Path)>;

    thread_local! {
        static HOOK: RefCell<Option<(Phase, Hook)>> = RefCell::new(None);
    }

    pub(crate) fn install<F>(phase: Phase, hook: F)
    where
        F: FnOnce(&Path) + 'static,
    {
        HOOK.with(|slot| *slot.borrow_mut() = Some((phase, Box::new(hook))));
    }

    pub(crate) fn clear() {
        HOOK.with(|slot| *slot.borrow_mut() = None);
    }

    pub(super) fn run(phase: Phase, path: &Path) {
        let hook = HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(expected, _)| *expected == phase)
            {
                slot.take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook(path);
        }
    }
}

/// Perform path-shape checks before the value reaches filesystem APIs.
///
/// The returned value is the only path accepted by the filesystem-resolution
/// phase below. Keeping this phase free of filesystem operations makes the
/// trust boundary explicit to both reviewers and CodeQL.
fn validate_pipeline_worker_log_directory_input(path: &Path) -> Result<PathBuf, OrbitError> {
    if !path.is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "pipeline worker log directory must be absolute: {}",
            path.display()
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(OrbitError::InvalidInput(format!(
            "pipeline worker log directory must not contain traversal components: {}",
            path.display()
        )));
    }

    Ok(path.to_path_buf())
}

/// Canonicalize the nearest existing ancestor of `parent` and rejoin any
/// missing suffix. Unrelated ancestor symlinks are resolved; a dangling
/// symlink fails closed when canonicalization reaches that component.
#[cfg(not(unix))]
fn canonical_pipeline_worker_log_parent(parent: &Path) -> Result<PathBuf, OrbitError> {
    let mut existing = parent.to_path_buf();
    let mut missing = Vec::<OsString>::new();
    loop {
        match existing.canonicalize() {
            Ok(mut canonical) => {
                for name in missing.into_iter().rev() {
                    canonical.push(name);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                };
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                }
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect pipeline worker log directory '{}': {error}",
                    existing.display()
                )));
            }
        }
    }
}

pub(crate) fn configure_pipeline_worker_stdio(
    command: &mut Command,
    logs_dir: &Path,
    run_id: &str,
) -> Result<PipelineWorkerLog, OrbitError> {
    #[cfg(unix)]
    let PipelineWorkerLogDirectory {
        path: logs_dir,
        directory,
    } = prepare_pipeline_worker_log_directory(logs_dir)?;

    #[cfg(not(unix))]
    let logs_dir = {
        let logs_dir = validated_pipeline_worker_log_directory(logs_dir)?;
        std::fs::create_dir_all(&logs_dir).map_err(|error| {
            OrbitError::Io(format!(
                "create pipeline worker log directory '{}': {error}",
                logs_dir.display()
            ))
        })?;
        validated_pipeline_worker_log_directory(&logs_dir)?
    };
    let log_path = pipeline_worker_log_path(&logs_dir, run_id)?;

    #[cfg(all(test, unix))]
    pipeline_worker_log_test_hook::run(
        pipeline_worker_log_test_hook::Phase::DirectoryReady,
        &logs_dir,
    );

    #[cfg(unix)]
    restrict_pipeline_worker_log_directory(&directory, &logs_dir)?;
    #[cfg(not(unix))]
    restrict_pipeline_worker_log_directory(&logs_dir)?;

    #[cfg(all(test, unix))]
    pipeline_worker_log_test_hook::run(
        pipeline_worker_log_test_hook::Phase::BeforeLogOpen,
        &log_path,
    );

    #[cfg(unix)]
    let mut log = open_pipeline_worker_log_at(
        &directory,
        log_path.file_name().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "pipeline worker log has no final component: {}",
                log_path.display()
            ))
        })?,
        &log_path,
    )?;
    #[cfg(not(unix))]
    let mut options = OpenOptions::new();
    #[cfg(not(unix))]
    options.create(true).append(true).read(true);
    #[cfg(not(unix))]
    let mut log = options.open(&log_path).map_err(|error| {
        OrbitError::Io(format!(
            "open pipeline worker log '{}': {error}",
            log_path.display()
        ))
    })?;
    #[cfg(unix)]
    restrict_pipeline_worker_log_file(&log, &log_path)?;
    #[cfg(not(unix))]
    restrict_pipeline_worker_log_file(&log_path)?;
    if let Some(profile) = pipeline_worker_profile_file(
        &logs_dir,
        run_id,
        std::env::var_os("LLVM_PROFILE_FILE").as_deref(),
    )? {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    write_pipeline_worker_spawn_banner(&mut log, command);
    let reader = log.try_clone().map_err(|error| {
        OrbitError::Io(format!(
            "clone pipeline worker log reader '{}': {error}",
            log_path.display()
        ))
    })?;
    let stdout = log.try_clone().map_err(|error| {
        OrbitError::Io(format!(
            "clone pipeline worker log '{}': {error}",
            log_path.display()
        ))
    })?;
    command.stdout(Stdio::from(stdout)).stderr(Stdio::from(log));
    Ok(PipelineWorkerLog {
        path: log_path,
        reader,
    })
}

pub(crate) struct PipelineWorkerLog {
    path: PathBuf,
    reader: File,
}

impl PipelineWorkerLog {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn write_pipeline_worker_spawn_banner(file: &mut File, command: &Command) {
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let cwd = command.get_current_dir().map_or_else(
        || "<inherit>".to_string(),
        |path| path.display().to_string(),
    );
    let _ = writeln!(
        file,
        "orbit pipeline worker spawn\nprogram: {}\nargs: {args}\ncwd: {cwd}",
        command.get_program().to_string_lossy()
    );
}

fn read_pipeline_worker_log_tail(file: &mut File) -> Option<String> {
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(PIPELINE_WORKER_LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let output = String::from_utf8_lossy(&bytes);
    let output = orbit_common::observability::logging::redact_event_text(output.trim());
    if output.is_empty() {
        None
    } else if start > 0 {
        Some(format!(
            "[truncated to final {PIPELINE_WORKER_LOG_TAIL_BYTES} bytes]\n{output}"
        ))
    } else {
        Some(output)
    }
}

#[cfg(unix)]
fn restrict_pipeline_worker_log_directory(directory: &File, path: &Path) -> Result<(), OrbitError> {
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } < 0 {
        let error = std::io::Error::last_os_error();
        Err(OrbitError::Io(format!(
            "restrict pipeline worker log directory '{}': {error}",
            path.display()
        )))
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn restrict_pipeline_worker_log_directory(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}

#[cfg(unix)]
fn restrict_pipeline_worker_log_file(file: &File, path: &Path) -> Result<(), OrbitError> {
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
        let error = std::io::Error::last_os_error();
        Err(OrbitError::Io(format!(
            "restrict pipeline worker log '{}': {error}",
            path.display()
        )))
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn restrict_pipeline_worker_log_file(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}

fn pipeline_run_is_runnable(runs: &[JobRun], run_id: &str, max_active_runs: u32) -> bool {
    let mut ordered = runs.to_vec();
    ordered.sort_by(|left, right| {
        left.scheduled_at
            .cmp(&right.scheduled_at)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    ordered
        .iter()
        .take(max_active_runs.max(1) as usize)
        .any(|run| run.run_id == run_id)
}

fn input_hash(input: &Value) -> String {
    let encoded = serde_json::to_vec(input).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded))
}

/// The durable input one workspace drain carries for its whole window.
///
/// Every key here is *omitted* unless the caller asked for it, so a run's
/// persisted input records only the deviations from the job's own defaults —
/// which is what makes an omitted option indistinguishable from the behavior
/// that predated it. Pure, so the durable contract this shape represents can
/// be asserted without submitting a run.
pub(crate) fn workspace_auto_run_input(
    for_seconds: Option<u64>,
    max_active_leaf_runs: Option<u32>,
    completion: crate::application::workflow::CompletionPolicy,
    allowed_crews: &[String],
) -> Result<Value, OrbitError> {
    if max_active_leaf_runs == Some(0) {
        return Err(OrbitError::InvalidInput(
            "concurrency must be at least 1".to_string(),
        ));
    }
    let mut input = serde_json::Map::new();
    input.insert(
        "for_seconds".to_string(),
        json!(for_seconds.unwrap_or_default()),
    );
    // [ORB-11187] Blanket authorization: the drain re-lists the backlog every
    // pass, so this policy governs every task admitted for the whole window,
    // not only the ones visible at submission.
    if completion.completes() {
        input.insert(
            "completion".to_string(),
            Value::String(completion.as_input_value().to_string()),
        );
    }
    if let Some(max_active_leaf_runs) = max_active_leaf_runs {
        input.insert(
            "max_active_leaf_runs".to_string(),
            json!(max_active_leaf_runs),
        );
    }
    // [ORB-11242] Carried by the run itself, so every pipeline it admits
    // inherits the same window without re-deriving it from configuration.
    if !allowed_crews.is_empty() {
        input.insert("allowed_crews".to_string(), json!(allowed_crews));
    }
    Ok(Value::Object(input))
}

/// Test-only substitute for the detached worker program.
///
/// Production re-execs `current_exe` at `job run-pipeline-worker <run_id>`. A
/// test binary must never re-exec itself: libtest reads the worker argv as test
/// filters and recurses through the whole suite. In-crate tests install a small
/// script here instead and assert on the submission path around it.
#[cfg(test)]
pub(crate) mod worker_command_override {
    use std::cell::RefCell;
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// Replaced with the submitted run id in every argv entry.
    pub(crate) const RUN_ID_PLACEHOLDER: &str = "{run_id}";

    thread_local! {
        static ARGV: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    /// Install `argv` as this thread's worker program until [`clear`].
    pub(crate) fn set<I, S>(argv: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let argv = argv.into_iter().map(Into::into).collect::<Vec<_>>();
        ARGV.with(|slot| *slot.borrow_mut() = Some(argv));
    }

    pub(crate) fn clear() {
        ARGV.with(|slot| *slot.borrow_mut() = None);
    }

    pub(crate) fn command(workspace: &Path, run_id: &str) -> Option<Command> {
        let argv = ARGV.with(|slot| slot.borrow().clone())?;
        let mut parts = argv
            .iter()
            .map(|part| part.replace(RUN_ID_PLACEHOLDER, run_id));
        let program = parts.next()?;
        let mut command = Command::new(program);
        command
            .args(parts)
            .current_dir(workspace)
            .stdin(Stdio::null());
        Some(command)
    }
}

#[cfg(test)]
pub(crate) mod worker_observer_read_counter {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{LazyLock, Mutex};

    use crate::OrbitRuntime;

    type StoreRun = (PathBuf, String);

    static COUNTS: LazyLock<Mutex<HashMap<StoreRun, usize>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) struct Counter {
        key: StoreRun,
    }

    fn key(runtime: &OrbitRuntime, run_id: &str) -> StoreRun {
        // Run IDs are local to a database. Its resolved path remains stable
        // across runtime clones while isolating independent temporary stores.
        (
            runtime.context.persistence().audit_db.clone(),
            run_id.to_string(),
        )
    }

    pub(crate) fn track(runtime: &OrbitRuntime, run_id: &str) -> Counter {
        let key = key(runtime, run_id);
        COUNTS
            .lock()
            .expect("test observer counters are not poisoned")
            .insert(key.clone(), 0);
        Counter { key }
    }

    pub(crate) fn record(runtime: &OrbitRuntime, run_id: &str) {
        if let Some(count) = COUNTS
            .lock()
            .expect("test observer counters are not poisoned")
            .get_mut(&key(runtime, run_id))
        {
            *count += 1;
        }
    }

    impl Counter {
        pub(crate) fn reads(&self) -> usize {
            *COUNTS
                .lock()
                .expect("test observer counters are not poisoned")
                .get(&self.key)
                .expect("tracked observer counter exists")
        }
    }

    impl Drop for Counter {
        fn drop(&mut self) {
            COUNTS
                .lock()
                .expect("test observer counters are not poisoned")
                .remove(&self.key);
        }
    }
}
