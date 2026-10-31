//! Read-only exploration a managed run delegates through `orbit.agent.invoke`
//! [ORB-16814].
//!
//! An implement or review-repair agent hands a read-only question to a cheap
//! crew from the explorer purpose pool and gets a bounded answer back, so the
//! expensive implementing crew spends its turns on the change rather than on
//! reading.
//!
//! # What this module owns
//!
//! The explorer branch of agent invocation, after
//! [`OrbitRuntime::admit_agent_invoke`](crate::OrbitRuntime) has routed a
//! managed run's caller here:
//!
//! * the switch and the caller check: an enabled workspace, a live run of
//!   this workspace that is not itself an invocation, and an implement or
//!   review-repair activity;
//! * the working directory, which must be inside the calling run's own
//!   worktree;
//! * the crew, drawn from (or named within) the explorer pool;
//! * the per-run budgets: calls, concurrent explorers and timeout;
//! * the bounded answer the caller receives.
//!
//! The run it submits is the sandboxed explore activity: no trusted-host
//! admission, the read-only `reviewer` filesystem profile, a read-only tool
//! allowlist and, where the provider has one, its read-only inner sandbox.
//! Admission draws only crews that have an executor OS sandbox this host
//! supports or a provider read-only mode, and refuses before any run exists
//! when none does; dispatch still refuses the provider before spawn when its
//! wrapper cannot start and it has no read-only mode [ORB-20566].
//! A sandboxed caller reaches this admission through its run's plugin broker,
//! so the explorer's worker starts beside the caller's sandbox, not inside it.
//! A refusal is an ordinary tool error; the calling worker continues.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::fs::path::orbit_scratch_dir;
use orbit_common::text::floor_char_boundary;
use orbit_store::contracts::JobRunQuery;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::activity_job::{
    EXPLORE_CALLER_ACTIVITIES, EXPLORE_JOB, EXPLORE_PARENT_RUN_KEY,
    explore_provider_has_read_only_mode,
};
use orbit_types::workflow::{ActivityCrewPoolMember, CrewPurpose, ExecutorSandboxKind, JobRun};
use serde::Serialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::contracts::{CrewCandidate, CrewPoolDraw};
use crate::application::job::crew_pools::random_crew_ticket;
use crate::application::job::log_best_effort;
use crate::application::job::pipeline::{PipelineInvokeResult, PipelineSubmission, input_hash};
use crate::runtime::authorization::ManagedRunCaller;
use crate::runtime::host_resource::delivery::RESOURCE_CLASS_KEY;

use super::invoke::{
    AGENT_INVOKE_JOB_ID, AgentInvokeRequest, AgentInvokeResult, AgentInvokeSubmission,
    require_non_empty,
};

/// Catalog job that carries one explorer invocation.
pub const EXPLORE_JOB_ID: &str = EXPLORE_JOB;

/// Run-input key carrying the task the delegating run delivers. Deliberately
/// not `task_id`: an explorer neither owns nor transitions the task, and the
/// run surfaces that read `task_id` as ownership must not count it.
const EXPLORE_PARENT_TASK_KEY: &str = "parent_task_id";

/// Run-input key carrying the activity that delegated the invocation.
const EXPLORE_PARENT_ACTIVITY_KEY: &str = "parent_activity";

/// The envelope `result` key an explorer lists the paths it read under.
const PATHS_READ_KEY: &str = "paths_read";

/// Run-input key carrying the caller's retry key.
const EXPLORE_IDEMPOTENCY_KEY: &str = "idempotency_key";

/// One submission of either invocation mode.
#[derive(Debug, Clone)]
pub enum AgentInvocation {
    /// An operator's trusted-host invocation.
    TrustedHost(AgentInvokeSubmission),
    /// A managed run's sandboxed read-only explorer.
    Explore(ExploreSubmission),
}

/// The durable outcome of an admitted explorer invocation.
#[derive(Debug, Clone, Serialize)]
pub struct ExploreSubmission {
    pub run_id: String,
    pub job_id: String,
    pub submitted_at: String,
    /// Whether the run is waiting on the job's `max_active_runs` ceiling.
    pub queued: bool,
    pub queue_position: Option<usize>,
    /// What the caller should treat as this submission's state.
    ///
    /// A new run is `queued` or `submitted`. A retried idempotency key reports
    /// `queued` while the original run is waiting for a slot, that run's own
    /// terminal state once it has finished, and `submitted` while it is still
    /// in flight and not waiting.
    pub state: String,
    /// Whether the caller's idempotency key resolved an earlier explorer of
    /// the same run instead of starting a new one.
    pub deduplicated: bool,
    /// The run that delegated the invocation.
    pub parent_run_id: String,
    /// The explorer crew.
    pub crew: String,
    /// The eligible members and weights frozen in the explorer run's input.
    pub eligible_pool: Vec<ActivityCrewPoolMember>,
    /// Effective wall-clock bound, in seconds.
    pub timeout_seconds: u64,
    /// The exact checkout root the explorer reads.
    pub workspace_path: String,
    /// The directory the caller asked the explorer to start from.
    pub cwd: String,
    /// The calling run's explorer calls, this one included, and its budget.
    pub calls_used: u32,
    pub calls_allowed: u32,
    /// The workspace's answer size cap, in bytes.
    pub max_answer_bytes: u32,
}

/// An explorer's answer as its caller receives it: the shared invocation
/// projection, bounded to the workspace's answer size cap.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BoundedExploreAnswer {
    pub result: AgentInvokeResult,
    /// Whether the answer was cut to fit `workflow.explore.max_answer_bytes`.
    /// The complete answer stays readable on the run.
    pub answer_truncated: bool,
}

/// One explorer a run delegated, as the run detail shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExploreInvocationSummary {
    pub run_id: String,
    /// The run's own state, not its provider's exit code.
    pub state: String,
    pub crew: Option<String>,
    /// The activity that delegated it.
    pub parent_activity: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    /// Provider-reported cost where every invocation reported one, else the
    /// price-table cost; `None` when neither covers the explorer's model.
    pub cost_usd: Option<f64>,
    /// How many paths its answer says it read; `None` before it answered.
    pub paths_read: Option<usize>,
}

impl OrbitRuntime {
    /// Admit and submit one explorer for a managed run's caller.
    ///
    /// Every check runs before anything durable exists, and each refusal
    /// names why. The budget reads and the insert share one lock in the
    /// parent's first existing worktree scratch, independent of which owned
    /// worktree holds `cwd`, so concurrent calls cannot both take its last slot.
    pub(crate) fn submit_explore_run(
        &self,
        caller: ManagedRunCaller,
        request: AgentInvokeRequest<'_>,
    ) -> Result<ExploreSubmission, OrbitError> {
        let actor = request
            .actor
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| self.actor_label().to_string(), ToOwned::to_owned);
        let result = self.admit_and_submit_explore(&caller, &request, &actor);
        log_best_effort(
            "record explorer invocation audit",
            result
                .as_ref()
                .map(|submission| submission.run_id.as_str())
                .unwrap_or_default(),
            self.record_pipeline_audit(
                "agent.explore",
                result
                    .as_ref()
                    .ok()
                    .map(|submission| submission.run_id.as_str()),
                Some(&actor),
                match &result {
                    Ok(_) => AuditEventStatus::Success,
                    Err(_) => AuditEventStatus::Failure,
                },
                json!({
                    "actor": actor,
                    "job_name": EXPLORE_JOB_ID,
                    "parent_run_id": caller.run_id,
                    "parent_activity": caller.activity,
                    "run_id": result.as_ref().ok().map(|submission| submission.run_id.clone()),
                    "crew": result.as_ref().ok().map(|submission| submission.crew.clone()),
                    "deduplicated": result.as_ref().ok().map(|submission| submission.deduplicated),
                }),
                result.as_ref().err().map(ToString::to_string),
            ),
        );
        result
    }

    fn admit_and_submit_explore(
        &self,
        caller: &ManagedRunCaller,
        request: &AgentInvokeRequest<'_>,
        actor: &str,
    ) -> Result<ExploreSubmission, OrbitError> {
        let mut settings = self.explore_settings();
        if !settings.enabled {
            return Err(OrbitError::CapabilityDenied(
                "read-only exploration is off for this workspace \
                 (`workflow.explore.enabled` is false); continue without delegating"
                    .to_string(),
            ));
        }
        let parent = self.get_job_run_backend(&caller.run_id)?.ok_or_else(|| {
            OrbitError::CapabilityDenied(
                "the calling run is not a run of this workspace; only a managed implement or \
                 review-repair run may delegate exploration"
                    .to_string(),
            )
        })?;
        if parent.job_id == EXPLORE_JOB_ID || parent.job_id == AGENT_INVOKE_JOB_ID {
            return Err(OrbitError::CapabilityDenied(
                "an invoked agent cannot invoke another agent".to_string(),
            ));
        }
        if parent.state.is_terminal() {
            return Err(OrbitError::CapabilityDenied(format!(
                "the calling run is {}; a finished run delegates nothing",
                parent.state
            )));
        }
        let activity = caller
            .activity
            .as_deref()
            .filter(|activity| EXPLORE_CALLER_ACTIVITIES.contains(activity))
            .ok_or_else(|| {
                OrbitError::CapabilityDenied(
                    "only an implement or review-repair agent may delegate exploration through \
                     `orbit.agent.invoke`"
                        .to_string(),
                )
            })?
            .to_string();
        if let Some(input) = &parent.input
            && let Some(task_id) = crate::runtime::run_input::singular_task_id_from_input(input)
        {
            settings.max_calls_per_run = self.experiment_explore_max_calls(
                input,
                task_id,
                &activity,
                settings.max_calls_per_run,
            )?;
        }
        if request
            .provider_sandbox
            .is_some_and(|mode| !mode.trim().is_empty())
        {
            return Err(OrbitError::InvalidInput(
                "an explorer's provider sandbox is always read-only; omit `provider_sandbox`"
                    .to_string(),
            ));
        }
        let prompt = require_non_empty(request.prompt, "prompt")?;
        let (root, cwd) = self.explore_checkout(&parent, request.cwd)?;
        let draw = self.explore_crew(request.crew)?;
        let crew = draw.crew.name;
        let timeout_seconds = explore_timeout(request.timeout_seconds, settings.timeout_seconds)?;
        let idempotency_key = request
            .idempotency_key
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let mut input = json!({
            "prompt": prompt,
            // The exact checkout root is both the child cwd and its declared
            // pair, which the engine validates against the registered
            // primary before it spawns anything.
            "workspace_path": root.display().to_string(),
            "repo_root": root.display().to_string(),
            "cwd": cwd.display().to_string(),
            "crew": crew,
            "eligible_pool": draw.eligible_pool,
            "timeout_seconds": timeout_seconds,
            EXPLORE_PARENT_RUN_KEY: parent.run_id,
            EXPLORE_PARENT_ACTIVITY_KEY: activity,
        });
        if let Some(task_id) = parent_task_id(&parent) {
            input[EXPLORE_PARENT_TASK_KEY] = json!(task_id);
        }
        // The explorer's minutes belong to the delegating delivery's profile
        // and take no slot of their own.
        if let Some(class) = parent
            .input
            .as_ref()
            .and_then(|parent_input| parent_input.get(RESOURCE_CLASS_KEY))
        {
            input[RESOURCE_CLASS_KEY] = class.clone();
        }
        if let Some(key) = idempotency_key {
            input[EXPLORE_IDEMPOTENCY_KEY] = json!(key);
        }

        // Pick the lock from the parent's ordered worktree paths, never the
        // requested cwd: a stale task-fallback worktree and the run-id worktree
        // must share the budget reads and insert under one lock. Worktree
        // scratch stays writable when the primary state directory is read-only.
        let lock_target = explore_admission_lock_target(&self.paths().repo_root, &parent)?;
        let admitted = with_exclusive_file_lock(&lock_target, "explorer admission", || {
            let prior = self.explore_runs_of(&parent)?;
            if let Some(existing) = idempotency_key.and_then(|key| {
                prior
                    .iter()
                    .find(|run| run_input_str(run, EXPLORE_IDEMPOTENCY_KEY) == Some(key))
            }) {
                // The retry's crew, timeout and directory were checked only so
                // an unusable call is still refused. The run that already
                // exists keeps the settings it was admitted with.
                let settings = persisted_explore_settings(existing)?;
                let (queued, queue_position, state) = self.explore_retry_queue(existing)?;
                return Ok(ExploreAdmission {
                    invoke: PipelineInvokeResult {
                        run_id: existing.run_id.clone(),
                        job_name: existing.job_id.clone(),
                        submitted_at: existing.scheduled_at.to_rfc3339(),
                        queued,
                        queue_position,
                    },
                    deduplicated: true,
                    calls_used: u32::try_from(prior.len()).unwrap_or(u32::MAX),
                    crew: settings.crew,
                    eligible_pool: settings.eligible_pool,
                    timeout_seconds: settings.timeout_seconds,
                    workspace_path: settings.workspace_path,
                    cwd: settings.cwd,
                    state,
                });
            }
            let calls = u32::try_from(prior.len()).unwrap_or(u32::MAX);
            if calls >= settings.max_calls_per_run {
                return Err(OrbitError::CapabilityDenied(format!(
                    "this run has used all {} explorer calls \
                         (`workflow.explore.max_calls_per_run`); continue without delegating",
                    settings.max_calls_per_run
                )));
            }
            let active = prior.iter().filter(|run| !run.state.is_terminal()).count();
            if u32::try_from(active).unwrap_or(u32::MAX) >= settings.max_concurrent_per_run {
                return Err(OrbitError::CapabilityDenied(format!(
                    "this run already has {active} explorer(s) running, its limit \
                         (`workflow.explore.max_concurrent_per_run`); wait for one to finish \
                         or continue without delegating"
                )));
            }
            let invoke = self.submit_persisted_pipeline_run(PipelineSubmission::catalog(
                EXPLORE_JOB_ID,
                input.clone(),
                Some(actor),
            ))?;
            let state = if invoke.queued { "queued" } else { "submitted" };
            Ok(ExploreAdmission {
                crew: crew.clone(),
                eligible_pool: draw.eligible_pool.clone(),
                timeout_seconds,
                workspace_path: root.display().to_string(),
                cwd: cwd.display().to_string(),
                state: state.to_string(),
                deduplicated: false,
                calls_used: calls.saturating_add(1),
                invoke,
            })
        })?;
        if !admitted.deduplicated {
            tracing::info!(
                target: "orbit.agent_explore",
                run_id = %admitted.invoke.run_id,
                parent_run_id = %parent.run_id,
                crew = %admitted.crew,
                input_hash = %input_hash(&input),
                "admitted a read-only explorer"
            );
        }
        Ok(ExploreSubmission {
            run_id: admitted.invoke.run_id,
            job_id: admitted.invoke.job_name,
            submitted_at: admitted.invoke.submitted_at,
            queued: admitted.invoke.queued,
            queue_position: admitted.invoke.queue_position,
            state: admitted.state,
            deduplicated: admitted.deduplicated,
            parent_run_id: parent.run_id,
            crew: admitted.crew,
            eligible_pool: admitted.eligible_pool,
            timeout_seconds: admitted.timeout_seconds,
            workspace_path: admitted.workspace_path,
            cwd: admitted.cwd,
            calls_used: admitted.calls_used,
            calls_allowed: settings.max_calls_per_run,
            max_answer_bytes: settings.max_answer_bytes,
        })
    }

    /// Queue placement of an explorer that already exists, and the state word
    /// a retry should report for it.
    ///
    /// Placement uses the same `max_active_runs` order as a fresh submission.
    /// A finished run is not waiting, so the word is its own terminal state.
    fn explore_retry_queue(
        &self,
        run: &JobRun,
    ) -> Result<(bool, Option<usize>, String), OrbitError> {
        let (_, spec) = self.load_v2_job_asset_by_name(EXPLORE_JOB_ID)?;
        let active = self
            .stores()
            .jobs()
            .list_pending_or_running_job_runs(EXPLORE_JOB_ID)?;
        let queue_position = super::super::pipeline::pipeline_run_queue_position(
            &active,
            &run.run_id,
            spec.max_active_runs,
        );
        let queued = queue_position.is_some();
        let state = if queued {
            "queued".to_string()
        } else if run.state.is_terminal() {
            run.state.to_string()
        } else {
            "submitted".to_string()
        };
        Ok((queued, queue_position, state))
    }

    /// The calling run's own checkout root, and the canonical `cwd` inside
    /// it. Any other directory — another run's worktree, the primary
    /// checkout, a path outside the workspace — is refused.
    fn explore_checkout(
        &self,
        parent: &JobRun,
        requested: &str,
    ) -> Result<(PathBuf, PathBuf), OrbitError> {
        let foreign = || {
            OrbitError::PolicyDenied(
                "`cwd` must be inside the calling run's own worktree".to_string(),
            )
        };
        let cwd = self.resolve_workspace_cwd("cwd", requested)?;
        let primary_root = self.paths().repo_root.canonicalize()?;
        let linked_root =
            crate::paths::find_linked_worktree_root(&cwd, &primary_root).ok_or_else(foreign)?;
        let owned = orbit_engine::run_worktree_paths(&self.paths().repo_root, parent)?
            .iter()
            .filter_map(|path| path.canonicalize().ok())
            .any(|path| path == linked_root);
        if !owned {
            return Err(foreign());
        }
        Ok((linked_root, cwd))
    }

    /// The explorer crew: the named one when the explorer pool holds it with
    /// a weight above 0, else a weighted draw over the pool. With no usable
    /// pool there is no explorer.
    fn explore_crew(&self, requested: Option<&str>) -> Result<CrewPoolDraw, OrbitError> {
        let (candidates, source) = self.bounded_explore_candidates()?;
        if let Some(named) = self.canonical_crew_name(requested)? {
            let selected = candidates
                .iter()
                .find(|candidate| candidate.crew.name == named && candidate.weight > 0)
                .cloned()
                .ok_or_else(|| {
                    OrbitError::CapabilityDenied(format!(
                        "crew '{named}' is not an explorer crew on this host, or cannot run \
                         read-only here; name one from `{}` or omit `crew`",
                        CrewPurpose::Explorer.config_key()
                    ))
                })?;
            return self.draw_crew_pool(vec![selected], &source, &mut random_crew_ticket);
        }
        self.draw_crew_pool(candidates, &source, &mut random_crew_ticket)
    }

    /// The explorer pool members that can run on this host under an enforced
    /// read-only boundary, or the one operator-facing reason none can
    /// [ORB-20566].
    ///
    /// A member qualifies when its provider has a read-only mode of its own,
    /// or its executor declares an OS sandbox this host has a backend for. A
    /// member with neither would be refused at dispatch after its run already
    /// existed, so it is never drawn and a pool of only such members refuses
    /// here, before anything durable exists. Whether the declared wrapper can
    /// actually start is still probed at dispatch, which refuses before spawn.
    fn bounded_explore_candidates(&self) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        let Some((candidates, source)) =
            self.purpose_pool_candidates(CrewPurpose::Explorer, false)?
        else {
            return Err(OrbitError::CapabilityDenied(format!(
                "no explorer crew can run on this host: `{}` is unset, disabled, or excluded \
                 by its executors' host_os",
                CrewPurpose::Explorer.config_key()
            )));
        };
        let mut unbounded = Vec::new();
        let mut bounded = Vec::new();
        for candidate in candidates {
            match self.explore_boundary_refusal(&candidate.crew.assignment.provider)? {
                None => bounded.push(candidate),
                Some(reason) => unbounded.push(format!("crew '{}': {reason}", candidate.crew.name)),
            }
        }
        if bounded.is_empty() {
            return Err(OrbitError::CapabilityDenied(format!(
                "no explorer crew can run under an enforced read-only sandbox on this host ({}); \
                 an operator must give an explorer crew's executor a sandbox this host \
                 supports, or add a crew whose provider has a read-only mode, to `{}`. \
                 Continue without delegating",
                unbounded.join("; "),
                CrewPurpose::Explorer.config_key()
            )));
        }
        Ok((bounded, source))
    }

    /// Why `provider` has no enforced read-only boundary on this host, or
    /// `None` when it has one.
    fn explore_boundary_refusal(&self, provider: &str) -> Result<Option<String>, OrbitError> {
        if explore_provider_has_read_only_mode(provider) {
            return Ok(None);
        }
        let host_os = std::env::consts::OS;
        Ok(
            match self.get_executor_def(provider)?.and_then(|def| def.sandbox) {
                Some(kind) if kind != ExecutorSandboxKind::Off && kind.is_available_on(host_os) => {
                    None
                }
                Some(ExecutorSandboxKind::Off) => Some(format!(
                    "executor `{provider}` declares `sandbox: off`, and the provider has no \
                     read-only mode"
                )),
                Some(kind) => Some(format!(
                    "executor `{provider}` declares `sandbox: {kind}`, which {host_os} cannot \
                     apply, and the provider has no read-only mode"
                )),
                None => Some(format!(
                    "executor `{provider}` declares no sandbox, and the provider has no \
                     read-only mode"
                )),
            },
        )
    }

    /// Whether an implement or review-repair run is offered read-only
    /// delegation: the switch is on and an explorer crew can run here under
    /// an enforced read-only boundary. The prompt says nothing of delegation
    /// otherwise.
    pub(crate) fn explore_delegation_offered(&self) -> bool {
        self.explore_settings().enabled && self.bounded_explore_candidates().is_ok()
    }

    /// Every explorer `parent` delegated, oldest first. Explorers are only
    /// ever admitted after their parent, so the scan starts there.
    fn explore_runs_of(&self, parent: &JobRun) -> Result<Vec<JobRun>, OrbitError> {
        let mut runs = self
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(EXPLORE_JOB_ID.to_string()),
                created_since: Some(parent.created_at),
                include_steps: false,
                ..JobRunQuery::default()
            })?
            .into_iter()
            .filter(|run| run_input_str(run, EXPLORE_PARENT_RUN_KEY) == Some(&parent.run_id))
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| run.created_at);
        Ok(runs)
    }

    /// The explorers `run_id` delegated, oldest first; empty for a run that
    /// delegated none.
    pub fn explore_invocations_of(&self, run_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        match self.get_job_run_backend(run_id)? {
            Some(parent) => self.explore_runs_of(&parent),
            None => Ok(Vec::new()),
        }
    }

    /// The explorers `run_id` delegated, oldest first, with what each cost.
    pub fn explore_invocation_summaries(
        &self,
        run_id: &str,
    ) -> Result<Vec<ExploreInvocationSummary>, OrbitError> {
        self.explore_invocations_of(run_id)?
            .into_iter()
            .map(|run| {
                let records = self.invocation_records(crate::InvocationQuery {
                    job_run_id: Some(run.run_id.clone()),
                    limit: 100,
                    ..crate::InvocationQuery::default()
                })?;
                let sum = |field: fn(&crate::InvocationRecord) -> u64| {
                    records.iter().map(field).sum::<u64>()
                };
                let provider_cost = records
                    .iter()
                    .map(|record| record.provider_cost_usd)
                    .sum::<Option<f64>>();
                let derived_cost = records
                    .iter()
                    .map(|record| record.derived_cost_usd)
                    .sum::<Option<f64>>();
                let paths_read = self
                    .read_run_state(&run.run_id)?
                    .as_ref()
                    .and_then(|state| state.step_output_entries().next_back())
                    .and_then(|(_, output)| output.get(PATHS_READ_KEY))
                    .and_then(Value::as_array)
                    .map(Vec::len);
                Ok(ExploreInvocationSummary {
                    state: run.state.to_string(),
                    crew: run_input_str(&run, "crew").map(ToOwned::to_owned),
                    parent_activity: run_input_str(&run, EXPLORE_PARENT_ACTIVITY_KEY)
                        .map(ToOwned::to_owned),
                    created_at: run.created_at.to_rfc3339(),
                    started_at: run.started_at.map(|at| at.to_rfc3339()),
                    finished_at: run.finished_at.map(|at| at.to_rfc3339()),
                    duration_ms: run.duration_ms,
                    input_tokens: sum(|record| record.input_tokens),
                    output_tokens: sum(|record| record.output_tokens),
                    total_tokens: sum(|record| record.total_tokens),
                    cost_usd: if records.is_empty() {
                        None
                    } else {
                        provider_cost.or(derived_cost)
                    },
                    paths_read,
                    run_id: run.run_id,
                })
            })
            .collect()
    }

    /// Bound an explorer's answer to `workflow.explore.max_answer_bytes`.
    ///
    /// The cut drops the least essential parts first — the final message,
    /// then extra result fields, then trailing findings and next steps — and
    /// only then shortens the summary and the paths read. The complete answer
    /// stays on the run for `orbit.workflow.run.show`. If final-message metadata
    /// alone exceeds the cap, the answer falls back to its minimal shape;
    /// the result still carries the complete stdout reference.
    pub fn bound_explore_answer(&self, mut result: AgentInvokeResult) -> BoundedExploreAnswer {
        let cap = usize::try_from(self.explore_settings().max_answer_bytes).unwrap_or(usize::MAX);
        let mut answer_truncated = false;
        if let Some(answer) = result.answer.as_mut() {
            let size = |answer: &crate::application::job::agent::invoke::AgentInvokeAnswer| {
                serde_json::to_vec(answer).map_or(usize::MAX, |bytes| bytes.len())
            };
            if size(answer) > cap {
                answer_truncated = true;
                if answer.final_message.take().is_some() {
                    answer.final_message_truncated = true;
                    answer.final_message_blob_ref = answer
                        .final_message_blob_ref
                        .clone()
                        .or_else(|| result.stdout_blob_ref.clone());
                }
            }
            if size(answer) > cap {
                answer.extra.clear();
            }
            while size(answer) > cap && answer.next_steps.pop().is_some() {}
            while size(answer) > cap && answer.findings.len() > 1 {
                answer.findings.pop();
            }
            while size(answer) > cap
                && answer
                    .paths_read
                    .as_mut()
                    .is_some_and(|paths| paths.pop().is_some())
            {}
            if size(answer) > cap {
                answer.findings.clear();
            }
            let overflow = size(answer).saturating_sub(cap);
            if overflow > 0
                && let Some(summary) = answer.summary.as_mut()
            {
                let keep = summary.len().saturating_sub(overflow);
                summary.truncate(floor_char_boundary(summary, keep));
            }
            if size(answer) > cap {
                // The admitted floor (167 bytes) holds this fixed shape, even
                // without a summary. Blob references have no length bound, so
                // a cap alone cannot guarantee that metadata fits.
                answer.summary = None;
                answer.paths_read = None;
                answer.final_message_bytes = None;
                answer.final_message_blob_ref = None;
            }
            // The legacy top-level summary mirrors the answer's.
            result.summary = answer.summary.clone();
        }
        if answer_truncated {
            result.preview = None;
            result.preview_truncated = true;
        }
        BoundedExploreAnswer {
            result,
            answer_truncated,
        }
    }
}

/// The per-parent explorer admission lock, independent of the requested `cwd`.
/// Use the first existing owned worktree in the engine's stable order: the
/// run-id worktree when present, otherwise the task-derived fallback. Calls
/// from either root therefore serialize on the same target. Keeping the lock
/// in worktree scratch also supports a read-only primary state directory.
fn explore_admission_lock_target(repo_root: &Path, parent: &JobRun) -> Result<PathBuf, OrbitError> {
    let worktree_root = orbit_engine::run_worktree_paths(repo_root, parent)?
        .into_iter()
        .find_map(|path| path.canonicalize().ok())
        .ok_or_else(|| {
            OrbitError::PolicyDenied(
                "the calling run has no existing worktree for explorer admission".to_string(),
            )
        })?;
    Ok(orbit_scratch_dir(worktree_root).join("agent_explore_admission"))
}

/// The task the delegating run delivers, when it names exactly one.
fn parent_task_id(parent: &JobRun) -> Option<&str> {
    let input = parent.input.as_ref()?;
    input
        .get("task_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .or_else(|| match input.get("task_ids").and_then(Value::as_array) {
            Some(ids) if ids.len() == 1 => ids[0].as_str(),
            _ => None,
        })
}

fn run_input_str<'a>(run: &'a JobRun, key: &str) -> Option<&'a str> {
    run.input
        .as_ref()
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
}

/// Settings a retried explorer reports: the run's persisted input, not the
/// retry that resolved it.
struct PersistedExploreSettings {
    crew: String,
    eligible_pool: Vec<ActivityCrewPoolMember>,
    timeout_seconds: u64,
    workspace_path: String,
    cwd: String,
}

/// One admitted explorer, new or resolved, ready for [`ExploreSubmission`].
struct ExploreAdmission {
    invoke: PipelineInvokeResult,
    deduplicated: bool,
    calls_used: u32,
    crew: String,
    eligible_pool: Vec<ActivityCrewPoolMember>,
    timeout_seconds: u64,
    workspace_path: String,
    cwd: String,
    state: String,
}

fn persisted_explore_settings(run: &JobRun) -> Result<PersistedExploreSettings, OrbitError> {
    let input = run.input.as_ref().ok_or_else(|| {
        OrbitError::Execution(format!(
            "deduplicated explorer '{}' has no persisted input",
            run.run_id
        ))
    })?;
    let required_str = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "deduplicated explorer '{}' has no persisted {key}",
                    run.run_id
                ))
            })
    };
    let timeout_seconds = input
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds >= 1)
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "deduplicated explorer '{}' has no persisted timeout",
                run.run_id
            ))
        })?;
    Ok(PersistedExploreSettings {
        crew: required_str("crew")?,
        eligible_pool: input
            .get("eligible_pool")
            .map(|value| {
                serde_json::from_value(value.clone()).map_err(|error| {
                    OrbitError::Execution(format!("invalid persisted explorer draw: {error}"))
                })
            })
            .transpose()?
            .unwrap_or_default(),
        timeout_seconds,
        workspace_path: required_str("workspace_path")?,
        cwd: required_str("cwd")?,
    })
}

/// The explorer's bound: the request when it is within the workspace's
/// `workflow.explore.timeout_seconds`, else that setting.
fn explore_timeout(requested: Option<u64>, ceiling: u32) -> Result<u64, OrbitError> {
    let ceiling = u64::from(ceiling);
    match requested {
        None => Ok(ceiling),
        Some(0) => Err(OrbitError::InvalidInput(
            "`timeout_seconds` must be at least 1".to_string(),
        )),
        Some(seconds) if seconds > ceiling => Err(OrbitError::InvalidInput(format!(
            "an explorer's `timeout_seconds` must be at most {ceiling} \
             (`workflow.explore.timeout_seconds`)"
        ))),
        Some(seconds) => Ok(seconds),
    }
}
