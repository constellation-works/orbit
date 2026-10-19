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
//! Dispatch refuses the provider before spawn when neither the executor
//! sandbox nor a read-only inner mode is enforced.
//! A refusal is an ordinary tool error; the calling worker continues.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::text::floor_char_boundary;
use orbit_store::contracts::JobRunQuery;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::activity_job::{
    EXPLORE_CALLER_ACTIVITIES, EXPLORE_JOB, EXPLORE_PARENT_RUN_KEY,
};
use orbit_types::workflow::{CrewPurpose, JobRun};
use serde::Serialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::crew_pools::{random_crew_ticket, weighted_draw};
use crate::application::job::log_best_effort;
use crate::application::job::pipeline::{PipelineInvokeResult, PipelineSubmission, input_hash};
use crate::runtime::authorization::ManagedRunCaller;
use crate::runtime::host_resource::delivery::RESOURCE_CLASS_KEY;

use super::agent_invoke::{
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
    /// Whether the caller's idempotency key resolved an earlier explorer of
    /// the same run instead of starting a new one.
    pub deduplicated: bool,
    /// The run that delegated the invocation.
    pub parent_run_id: String,
    /// The explorer crew.
    pub crew: String,
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
    /// names why. The budget reads and the insert share one lock, so two
    /// concurrent calls from the same run cannot both take its last slot.
    pub(super) fn submit_explore_run(
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
        let settings = self.explore_settings();
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
        let crew = self.explore_crew(request.crew)?;
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

        let lock_target = self.paths().state_dir.join("agent_explore_admission");
        let (invoke, deduplicated, calls_used) =
            with_exclusive_file_lock(&lock_target, "explorer admission", || {
                let prior = self.explore_runs_of(&parent)?;
                if let Some(existing) = idempotency_key.and_then(|key| {
                    prior
                        .iter()
                        .find(|run| run_input_str(run, EXPLORE_IDEMPOTENCY_KEY) == Some(key))
                }) {
                    return Ok::<_, OrbitError>((
                        resolved_invocation(existing),
                        true,
                        u32::try_from(prior.len()).unwrap_or(u32::MAX),
                    ));
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
                Ok((invoke, false, calls.saturating_add(1)))
            })?;
        if !deduplicated {
            tracing::info!(
                target: "orbit.agent_explore",
                run_id = %invoke.run_id,
                parent_run_id = %parent.run_id,
                crew = %crew,
                input_hash = %input_hash(&input),
                "admitted a read-only explorer"
            );
        }
        Ok(ExploreSubmission {
            run_id: invoke.run_id,
            job_id: invoke.job_name,
            submitted_at: invoke.submitted_at,
            queued: invoke.queued,
            queue_position: invoke.queue_position,
            deduplicated,
            parent_run_id: parent.run_id,
            crew,
            timeout_seconds,
            workspace_path: root.display().to_string(),
            cwd: cwd.display().to_string(),
            calls_used,
            calls_allowed: settings.max_calls_per_run,
            max_answer_bytes: settings.max_answer_bytes,
        })
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
    fn explore_crew(&self, requested: Option<&str>) -> Result<String, OrbitError> {
        let Some((candidates, source)) =
            self.purpose_pool_candidates(CrewPurpose::Explorer, false)?
        else {
            return Err(OrbitError::CapabilityDenied(format!(
                "no explorer crew can run on this host: `{}` is unset, disabled, or excluded \
                 by its executors' host_os",
                CrewPurpose::Explorer.config_key()
            )));
        };
        if let Some(named) = self.canonical_crew_name(requested)? {
            return candidates
                .iter()
                .find(|candidate| candidate.crew.name == named && candidate.weight > 0)
                .map(|candidate| candidate.crew.name.clone())
                .ok_or_else(|| {
                    OrbitError::CapabilityDenied(format!(
                        "crew '{named}' is not an explorer crew on this host; name one from \
                         `{}` or omit `crew`",
                        CrewPurpose::Explorer.config_key()
                    ))
                });
        }
        weighted_draw(&candidates, &source, &mut random_crew_ticket)
            .map(|candidate| candidate.crew.name.clone())
    }

    /// Whether an implement or review-repair run is offered read-only
    /// delegation: the switch is on and an explorer crew can run here. The
    /// prompt says nothing of delegation otherwise.
    pub(crate) fn explore_delegation_offered(&self) -> bool {
        self.explore_settings().enabled
            && matches!(
                self.purpose_pool_candidates(CrewPurpose::Explorer, false),
                Ok(Some(_))
            )
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
    /// stays on the run for `orbit.workflow.run.show`.
    pub fn bound_explore_answer(&self, mut result: AgentInvokeResult) -> BoundedExploreAnswer {
        let cap = usize::try_from(self.explore_settings().max_answer_bytes).unwrap_or(usize::MAX);
        let mut answer_truncated = false;
        if let Some(answer) = result.answer.as_mut() {
            let size = |answer: &crate::application::job::agent_invoke::AgentInvokeAnswer| {
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

fn resolved_invocation(run: &JobRun) -> PipelineInvokeResult {
    PipelineInvokeResult {
        run_id: run.run_id.clone(),
        job_name: run.job_id.clone(),
        submitted_at: run.scheduled_at.to_rfc3339(),
        queued: false,
        queue_position: None,
    }
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
