//! Explicit owner-local recovery. Generic resume remains deliberately unchanged.
mod adoption;
mod publication;

use chrono::Utc;
use orbit_common::fs::git::run_git;
use orbit_common::protocol::tool_input::required_string;
use orbit_common::security::release::sha256_hex;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{
    CommitObservationStatus, PR_AUTO_FIX_BUDGET, ReviewAdmission, SOURCE_REPAIR_KEY,
    SOURCE_REPAIR_LINKED, SOURCE_REPAIR_RESERVED, SourceRepairAdmission, SourceRepairBinding,
    pr_auto_fix_attempts,
};
use serde::Deserialize;
use serde_json::json;

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    action: String,
    task_id: String,
    request_key: Option<String>,
    binding: Option<SourceRepairBinding>,
    diagnosis: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairLink {
    request_key: String,
    run_id: String,
}

fn refuse(message: impl Into<String>) -> OrbitError {
    OrbitError::JobValidation(format!("source repair refused: {}", message.into()))
}

fn digest(value: &impl serde::Serialize) -> Result<String, OrbitError> {
    serde_json::to_vec(value)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|error| refuse(format!("unreadable evidence: {error}")))
}

fn task_digest(task: &Task) -> Result<String, OrbitError> {
    digest(&json!({
        "meaning": orbit_automation::review::task_meaning_digest(task)
            .map_err(|e| refuse(e.to_string()))?,
        "required_tools": task.required_tools,
        "crew": task.crew,
        "crew_source": task.crew_source,
        "complexity": task.complexity,
    }))
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, OrbitError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| refuse(format!("missing checkpoint field {name}")))
}

/// Corroborate an authenticated publication without selecting authority from
/// reference order. Historical PRs remain intact; contradictory identities do
/// not become valid merely because another reference matches.
fn task_pr_association(task: &Task, number: &str, url: &str) -> Result<(), OrbitError> {
    let mut associated = false;
    for reference in task
        .external_refs
        .iter()
        .filter(|reference| reference.system == orbit_types::task::GITHUB_PR_EXTERNAL_REF_SYSTEM)
    {
        if reference.id == number {
            // Key-only references are a supported persisted format. Repository
            // authority comes from the host checkpoints, origin and provider.
            if reference.url.as_deref().is_some_and(|value| value != url) {
                return Err(refuse(
                    "task PR association has a conflicting repository or URL",
                ));
            }
            associated = true;
        } else if reference.url.as_deref() == Some(url) {
            return Err(refuse("task PR association has a conflicting number"));
        }
    }
    if !associated {
        return Err(refuse(
            "task PR association is missing for the authenticated publication",
        ));
    }
    Ok(())
}

fn history(runtime: &OrbitRuntime, task: &str) -> Result<Vec<SourceRepairAdmission>, OrbitError> {
    orbit_types::workflow::source_repair_reservations(&runtime.get_task_history(task)?)
        .map_err(|error| refuse(format!("unreadable repair reservation: {error}")))
}

impl OrbitRuntime {
    pub(crate) fn validate_operator_source_repair(&self, run_id: &str) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            return Err(refuse(
                "claimed executions cannot apply operator source repair",
            ));
        }
        let run = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| refuse("repair run missing"))?;
        let record: SourceRepairAdmission = serde_json::from_value(
            run.input
                .as_ref()
                .and_then(|i| i.get(SOURCE_REPAIR_KEY))
                .cloned()
                .ok_or_else(|| refuse("admission missing"))?,
        )
        .map_err(|e| refuse(e.to_string()))?;
        let binding = &record.binding;
        let task = self.get_task(&binding.task_id)?;
        let source = self
            .get_job_run_backend(&binding.source_run_id)?
            .ok_or_else(|| refuse("source disappeared"))?;
        let state = self
            .read_run_state(&binding.source_run_id)?
            .ok_or_else(|| refuse("source checkpoints disappeared"))?;
        let definition = self.source_repair_definition(&source)?;
        if binding.schema_version != 1
            || binding.workspace_id != self.workspace_id()?
            || run
                .executed_on
                .as_ref()
                .is_none_or(|origin| origin.machine_id != binding.machine_id)
            || task
                .job_run_machine
                .as_ref()
                .is_none_or(|origin| origin.machine_id != binding.machine_id)
            || run.job_id != "task_pr_pipeline"
            || !matches!(run.state, JobRunState::Pending | JobRunState::Running)
            || run.retry_source_run_id.as_deref() != Some(binding.source_run_id.as_str())
            || task.job_run_id.as_deref() != Some(run_id)
            || task.status != TaskStatus::InProgress
            || self.automation_machine_identity() != Some(binding.machine_id.as_str())
            || source.state != JobRunState::Cancelled
            || run_owner_liveness(&source) != RunOwnerLiveness::Stopped
            || digest(
                &json!({"run":source,"steps":source.steps,"checkpoints":state,"definition":definition}),
            )? != binding.source_digest
            || task_digest(&task)? != binding.task_digest
            || self.stores().jobs().local_pull_for_run(run_id)?.is_some()
            || self
                .resolve_execution_claims()?
                .iter()
                .any(|claim| claim.claim.task_id == task.id)
            || !history(self, &task.id)?
                .iter()
                .any(|r| digest(r).ok() == digest(&record).ok())
        {
            return Err(refuse(
                "source, task, scope or reservation changed before source application",
            ));
        }
        task_pr_association(&task, &binding.pull_request, &binding.pull_request_url)?;
        Ok(())
    }

    pub(crate) fn source_repair_request(
        &self,
        session: &ToolSessionContext,
        input: &Value,
    ) -> Result<Value, OrbitError> {
        if orbit_common::governance::authorization::agent_context_declared()
            || self.worker_invocation().is_some()
        {
            return Err(OrbitError::CapabilityDenied(
                "source repair requires an operator on the owning host".into(),
            ));
        }
        let authority = self.admit_operator_operation("orbit.workflow.run.resume", session)?;
        let actor = authority
            .remote_caller_machine_id
            .clone()
            .map_or_else(|| self.actor().resolve_write_label(None, None), Ok)?;
        let source_id = required_string(input, &["id"], "id")?;
        let request: Request = serde_json::from_value(input["source_repair"].clone())
            .map_err(|error| OrbitError::InvalidInput(format!("invalid source_repair: {error}")))?;
        self.require_workspace_claim(
            "orbit.workflow.run.resume",
            input.get("claim_token").and_then(Value::as_str),
        )?;
        match request.action.as_str() {
            "inspect" => {
                let (_, _, binding) =
                    self.observe_source_repair(&source_id, &request.task_id, false)?;
                let mut fresh = json!({"task_ids":[request.task_id]});
                crate::application::review::install_review_admission(
                    self,
                    "task_pr_pipeline",
                    &mut fresh,
                    None,
                    false,
                )?;
                Ok(
                    json!({"binding":binding, "review":fresh[orbit_types::workflow::REVIEW_ADMISSION_KEY],
                    "reservations":history(self, &request.task_id)?}),
                )
            }
            "submit" => {
                self.ensure_coordination_task_write_permitted()?;
                let mut result = None;
                self.stores()
                    .tasks()
                    .with_task_write_lock(&request.task_id, &mut || {
                        result = Some(self.submit_source_repair(
                            &source_id,
                            &request,
                            &actor,
                            &authority.provenance.to_string(),
                        )?);
                        Ok(())
                    })?;
                result.ok_or_else(|| refuse("admission lock did not execute"))
            }
            _ => Err(OrbitError::InvalidInput(
                "source_repair.action must be inspect or submit".into(),
            )),
        }
    }

    fn submit_source_repair(
        &self,
        source_id: &str,
        request: &Request,
        actor: &str,
        provenance: &str,
    ) -> Result<Value, OrbitError> {
        let key = request
            .request_key
            .as_deref()
            .ok_or_else(|| refuse("request_key is required"))?;
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        {
            return Err(refuse(
                "request_key must be 1-128 ASCII letters, digits, '-', '_', '.' or ':'",
            ));
        }
        let binding = request
            .binding
            .as_ref()
            .ok_or_else(|| refuse("inspect and supply the exact binding"))?;
        if binding.source_run_id != source_id || binding.task_id != request.task_id {
            return Err(refuse(
                "request and binding identify different source or task",
            ));
        }
        let diagnosis = request.diagnosis.as_deref().filter(|s| !s.trim().is_empty() && s.len() <= 32768)
            .ok_or_else(|| refuse("diagnosis must name observed failures and remaining source work (1-32768 bytes)"))?;
        let reservations = history(self, &request.task_id)?;
        let prior = reservations.iter().find(|record| record.request_key == key);
        if prior.is_some_and(|record| record.binding != *binding || record.diagnosis != diagnosis) {
            return Err(refuse("request_key already reserved different input"));
        }
        // Reconcile an interrupted reply before re-observing a task the admitted
        // worker may already have rebound. A replay returns evidence, never starts
        // another worker or grants authority to a terminal repair.
        if let Some(record) = prior {
            let links = self.get_task_history(&request.task_id)?;
            let linked = links
                .iter()
                .filter(|entry| entry.event == SOURCE_REPAIR_LINKED)
                .map(|entry| {
                    serde_json::from_str::<RepairLink>(entry.note.as_deref().unwrap_or_default())
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| refuse(format!("unreadable repair link: {error}")))?;
            let ids = linked
                .iter()
                .filter(|link| link.request_key == key)
                .map(|link| link.run_id.clone())
                .collect::<BTreeSet<_>>();
            if ids.len() > 1 {
                return Err(refuse("repair key has inconsistent run links"));
            }
            let run = match ids.into_iter().next() {
                Some(id) => Some(
                    self.get_job_run_backend(&id)?
                        .ok_or_else(|| refuse("linked repair run is missing"))?,
                ),
                None => {
                    // A crash can lose the post-insert link even after many
                    // older generic retries. Read the normal lineage bound,
                    // and refuse uncertainty rather than admitting twice.
                    let children = self
                        .stores()
                        .jobs()
                        .job_run_retries(source_id, RESUME_LINEAGE_SCAN_LIMIT)?;
                    let at_bound = children.len() == RESUME_LINEAGE_SCAN_LIMIT;
                    let found = children.into_iter().find(|run| {
                        run.input
                            .as_ref()
                            .is_some_and(|input| input[SOURCE_REPAIR_KEY]["request_key"] == key)
                    });
                    if found.is_none() && at_bound {
                        return Err(refuse(
                            "repair linkage exceeds the bounded reconciliation window",
                        ));
                    }
                    found
                }
            };
            if let Some(run) = run {
                if run.retry_source_run_id.as_deref() != Some(source_id)
                    || run
                        .input
                        .as_ref()
                        .and_then(|i| i.get(SOURCE_REPAIR_KEY))
                        .map(digest)
                        .transpose()?
                        != Some(digest(record)?)
                {
                    return Err(refuse("linked repair evidence changed"));
                }
                return Ok(
                    json!({"run_id":run.run_id,"retry_source_run_id":source_id,"state":run.state,"replayed":true}),
                );
            }
        }
        let (source, task, observed) =
            self.observe_source_repair(source_id, &request.task_id, prior.is_some())?;
        if observed != *binding {
            return Err(refuse(
                "binding is stale; inspect the exact source, scope and lease again",
            ));
        }
        let admission = match prior {
            Some(record) => record.clone(),
            None => {
                let spent =
                    pr_auto_fix_attempts(&self.get_task_history(&task.id)?, &binding.pull_request)
                        .len()
                        + reservations
                            .iter()
                            .filter(|r| r.binding.pull_request_url == binding.pull_request_url)
                            .count();
                if spent >= PR_AUTO_FIX_BUDGET as usize {
                    return Err(refuse("pull request repair budget exhausted"));
                }
                if reservations.iter().any(|r| {
                    r.binding.pull_request_url == binding.pull_request_url
                        && r.binding.head_sha == binding.head_sha
                }) {
                    return Err(refuse(
                        "this published head already has a repair reservation; replay its request_key",
                    ));
                }
                let mut fresh = json!({"task_ids":[task.id]});
                crate::application::review::install_review_admission(
                    self,
                    "task_pr_pipeline",
                    &mut fresh,
                    None,
                    false,
                )?;
                let review: ReviewAdmission = serde_json::from_value(
                    fresh[orbit_types::workflow::REVIEW_ADMISSION_KEY].clone(),
                )
                .map_err(|error| refuse(error.to_string()))?;
                if self.workflow_required_validation_commands().is_empty() {
                    return Err(refuse(
                        "source repair requires current required validation commands",
                    ));
                }
                let record = SourceRepairAdmission {
                    request_key: key.into(),
                    binding: binding.clone(),
                    diagnosis: diagnosis.into(),
                    authorized_by: actor.into(),
                    authorizer_provenance: provenance.into(),
                    authorized_at: Utc::now(),
                    attempt: spent as u32 + 1,
                    review,
                };
                self.source_repair_event(
                    &task.id,
                    SOURCE_REPAIR_RESERVED,
                    serde_json::to_value(&record).map_err(|e| refuse(e.to_string()))?,
                )?;
                record
            }
        };
        // A reservation whose insertion was interrupted retains its charge and
        // captured policy. It cannot silently grant old policy after a config edit.
        let mut fresh = json!({"task_ids":[task.id]});
        crate::application::review::install_review_admission(
            self,
            "task_pr_pipeline",
            &mut fresh,
            None,
            false,
        )?;
        let mut current: ReviewAdmission =
            serde_json::from_value(fresh[orbit_types::workflow::REVIEW_ADMISSION_KEY].clone())
                .map_err(|e| refuse(e.to_string()))?;
        current.captured_at = admission.review.captured_at;
        if digest(&current)? != digest(&admission.review)? {
            return Err(refuse(
                "current review/validation policy changed after reservation",
            ));
        }
        // Build new input from a small allowlist. No completion, auto-fix,
        // certification, crew draw or checkpoint from the old run survives.
        let input = json!({"task_ids":[task.id],"base_branch":binding.base_branch,"base_sync":"remote",
            "completion":"review","auto_fix":false,orbit_types::workflow::REVIEW_ADMISSION_KEY:admission.review,SOURCE_REPAIR_KEY:admission});
        // Normal admission resolves the current task crew with its original
        // assignment provenance. Copying task.crew into an explicit run override
        // would bypass provider holds and turn a pool/default into operator intent.
        let (ancestors, lineage) = self.resume_lineage_run_ids(&source)?;
        let plan = ResumePlan {
            attempt: source.attempt.saturating_add(1),
            source,
            input: input.clone(),
            resume_state: None,
            lineage,
            ancestors,
            review_checkpoints: BTreeMap::new(),
            checkpoint_batch_id: None,
            pinned_definition: None,
        };
        // Review -> in-progress is a new operator source decision, not restored
        // completion authority. Its reservation is already durable above.
        if task.status == TaskStatus::Review {
            self.apply_task_automation_update(&task.id, TaskAutomationUpdate {
                status:Some(TaskStatus::InProgress),expected_status:Some(TaskStatus::Review),
                status_event:Some("source_repair_readmitted".into()),status_note:Some(format!("operator source repair request {key}; source={source_id}; completion=review")),
                ..Default::default()
            })?;
        }
        let submitted = self.submit_persisted_pipeline_run(PipelineSubmission {
            resume: Some(&plan),
            ..PipelineSubmission::catalog("task_pr_pipeline", input, Some(actor))
        })?;
        self.source_repair_event(
            &task.id,
            SOURCE_REPAIR_LINKED,
            json!({"request_key":key,"run_id":submitted.run_id}),
        )?;
        Ok(
            json!({"run_id":submitted.run_id,"retry_source_run_id":source_id,"replayed":false,"completion":"review"}),
        )
    }

    fn source_repair_event(&self, task: &str, event: &str, value: Value) -> Result<(), OrbitError> {
        self.apply_task_automation_update(
            task,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: event.into(),
                    note: Some(value.to_string()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
    }

    fn source_repair_definition(&self, source: &JobRun) -> Result<JobV2, OrbitError> {
        let mut job = match self.read_run_definition_snapshot(&source.run_id)? {
            Some((job, _)) => job,
            None => self.load_v2_job_asset_by_name(&source.job_id)?.1,
        };
        if !job.holds_task_delivery() {
            return Err(refuse("source definition is not a task delivery"));
        }
        orbit_engine::resolve_job_catalog_refs_for_execution(
            &mut job,
            &self
                .v2_activity_catalog()
                .map_err(|e| refuse(e.to_string()))?,
        )
        .map_err(|e| refuse(e.to_string()))?;
        Ok(job)
    }

    fn observe_source_repair(
        &self,
        source_id: &str,
        task_id: &str,
        reserved: bool,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        let source = self
            .get_job_run_backend(source_id)?
            .ok_or_else(|| refuse("source run missing"))?;
        let task = self.get_task(task_id)?;
        let local = self
            .automation_machine_identity()
            .ok_or_else(|| refuse("local machine identity unavailable"))?;
        if source.state != JobRunState::Cancelled
            || source.finished_at.is_none()
            || source.job_id != "task_pr_pipeline"
            || run_owner_liveness(&source) != RunOwnerLiveness::Stopped
        {
            return Err(refuse(
                "requires a cancelled task PR pipeline with a conclusively stopped worker",
            ));
        }
        if source
            .executed_on
            .as_ref()
            .is_none_or(|origin| origin.machine_id != local)
            || task
                .job_run_machine
                .as_ref()
                .is_none_or(|origin| origin.machine_id != local)
            || task.job_run_id.as_deref() != Some(source_id)
            || self
                .stores()
                .jobs()
                .local_pull_for_run(source_id)?
                .is_some()
            || self
                .resolve_execution_claims()?
                .iter()
                .any(|claim| claim.claim.task_id == task_id)
            || source.input.as_ref().is_none_or(|input| {
                input.get("claimed").and_then(Value::as_bool) == Some(true)
                    || carried_task_ids(input) != BTreeSet::from([task_id.to_string()])
            })
        {
            return Err(refuse(
                "requires the exact owner-local single-task run; foreign, rebound and claimed deliveries are refused",
            ));
        }
        if task.status != TaskStatus::Review && !(reserved && task.status == TaskStatus::InProgress)
        {
            return Err(refuse(
                "task must remain in review (or this reserved admission's in-progress stage)",
            ));
        }
        let state = self
            .read_run_state(source_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        if state.run_id != source_id || state.job_id != source.job_id {
            return Err(refuse("source checkpoint identity or claim mismatch"));
        }
        let definition = self.source_repair_definition(&source)?;
        let job = &definition;
        let checkpoint = |id: &str, action: &str| -> Result<&Value, OrbitError> {
            let index = job
                .steps
                .iter()
                .position(|step| step.id == id)
                .ok_or_else(|| refuse(format!("source definition lacks {id}")))?
                as u32;
            if !matches!(&job.steps[index as usize].body,
                JobV2StepBody::Target(target)
                    if target.activity_name.as_deref() == Some(action)
                    && matches!(&target.spec, ActivityV2Spec::Deterministic(spec) if spec.action == action))
            {
                return Err(refuse(format!(
                    "{id} is not the authenticated host {action} action"
                )));
            }
            let output = state
                .pipeline
                .get(id)
                .ok_or_else(|| refuse(format!("source lacks {id} evidence")))?;
            if state.step_states.get(&index) != Some(&JobRunState::Success)
                || state.step_output(index) != Some(output)
            {
                return Err(refuse(format!(
                    "{id} checkpoint is not authenticated success"
                )));
            }
            Ok(output)
        };
        let worktree = checkpoint("worktree", "worktree_setup")?;
        let push = checkpoint("push", "git_push")?;
        let pr = checkpoint("pr_open", "pr_open")?;
        let sync = checkpoint("sync_base", "git_rebase")?;
        let prepared = checkpoint("prepare_branch", "pr_prepare")?;
        let adopted = if source
            .input
            .as_ref()
            .is_some_and(|input| input.get(SOURCE_REPAIR_KEY).is_some())
        {
            Some(self.source_repair_adopted_binding(
                &source,
                &task,
                worktree,
                checkpoint("resume_candidate", "candidate_resume")?,
            )?)
        } else {
            None
        };
        // Setup precedes adoption and keeps its per-run branch. Only the
        // authenticated operator continuation may replace that branch identity.
        let branch = match &adopted {
            Some(binding) => binding.branch.as_str(),
            None => field(worktree, "head_ref")?,
        };
        let pushed = field(push, "local_sha")?;
        let synced = field(sync, "head_sha")?;
        let head = publication::published_head(
            job,
            &state,
            &checkpoint,
            sync,
            push,
            branch,
            &self.paths().repo_root,
        )?;
        let base_sha = field(sync, "base_sha")?;
        let base_branch = field(sync, "base")?;
        let number = field(pr, "pr_number")?;
        if adopted.as_ref().is_some_and(|binding| {
            binding.pull_request != number
                || binding.base_branch != base_branch
                || prepared.get("remote_sha").and_then(Value::as_str)
                    != Some(binding.head_sha.as_str())
                || pr.get("pr_url").and_then(Value::as_str)
                    != Some(binding.pull_request_url.as_str())
        }) || field(worktree, "job_run_id")? != source_id
            || field(prepared, "phase")? != "prepare"
            || field(sync, "phase")? != "rebase"
            || field(push, "phase")? != "push"
            || field(pr, "phase")? != "pr_open"
            || field(prepared, "head")? != branch
            || field(sync, "head")? != branch
            || field(push, "branch")? != branch
            || field(pr, "head")? != branch
            || field(prepared, "head_sha")? != field(sync, "head_sha_before")?
            || field(prepared, "base")? != base_branch
            || field(pr, "base")? != base_branch
            || field(prepared, "base_sha")? != base_sha
            || field(pr, "base_sha")? != base_sha
            || field(prepared, "base_ref")? != field(sync, "base_ref")?
            || field(pr, "base_ref")? != field(sync, "base_ref")?
            || prepared.get("remote_sha").is_none()
            || prepared.get("remote_sha") != sync.get("remote_sha_before")
            || push.get("remote_sha_before").is_none()
            || (push.get("remote_sha_before") != sync.get("remote_sha_before")
                && push.get("remote_sha_before").and_then(Value::as_str) != Some(pushed))
        {
            return Err(refuse("task, source branch and published PR disagree"));
        }
        let commit = self.observe_task_delivery(task_id, Some(source_id))?.commit;
        if !matches!(
            commit.status,
            CommitObservationStatus::Committed | CommitObservationStatus::AlreadyCommitted
        ) || commit.head_sha.is_none()
            || commit.base_sha.as_deref() != Some(field(worktree, "base_sha")?)
        {
            return Err(refuse("source has no authenticated host commit"));
        }
        let repo = &self.paths().repo_root;
        let git = |args: &[&str]| -> Result<String, OrbitError> {
            let output = orbit_common::fs::git::run_git_within(
                repo,
                args,
                if args.first() == Some(&"ls-remote") {
                    orbit_common::fs::git::GIT_REMOTE_TIMEOUT
                } else {
                    orbit_common::fs::git::GIT_LOCAL_TIMEOUT
                },
            )?;
            if !output.success {
                return Err(refuse(format!("Git observation failed: {}", output.stderr)));
            }
            Ok(output.stdout.trim().to_string())
        };
        for sha in [
            synced,
            pushed,
            head,
            base_sha,
            commit.head_sha.as_deref().unwrap_or_default(),
        ] {
            if sha.len() != 40
                || !sha.bytes().all(|b| b.is_ascii_hexdigit())
                || git(&["rev-parse", "--verify", &format!("{sha}^{{commit}}")])? != sha
            {
                return Err(refuse(
                    "candidate/base/commit object is unavailable or inexact",
                ));
            }
        }
        let committed = commit
            .head_sha
            .as_deref()
            .ok_or_else(|| refuse("commit head missing"))?;
        let before = field(prepared, "head_sha")?;
        // The host may rebase a commit: Git ancestry across that rewrite is
        // intentionally absent. Authenticate the entire successful host chain
        // rather than treating a caller's after-head as provenance.
        if git(&["rev-parse", "--verify", &format!("{before}^{{commit}}")])? != before
            || !run_git(repo, &["merge-base", "--is-ancestor", committed, before])?.success
            || !run_git(repo, &["merge-base", "--is-ancestor", base_sha, synced])?.success
            || sync.get("rewritten").and_then(Value::as_bool) != Some(before != synced)
            || (before != synced
                && prepared.get("sync_required").and_then(Value::as_bool) != Some(true))
        {
            return Err(refuse(
                "authenticated prepare/rebase commit chain is inconsistent",
            ));
        }
        if git(&["rev-parse", "--verify", &format!("refs/heads/{branch}")])? != head {
            return Err(refuse("retained local branch moved"));
        }
        if git(&[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{branch}"),
        ])?
        .split_whitespace()
        .next()
            != Some(head)
        {
            return Err(refuse("published branch lease drifted or disappeared"));
        }
        let remote_url = git2::Repository::open(repo)
            .and_then(|repo| repo.config()?.get_string("remote.origin.url"))
            .map_err(|e| refuse(format!("origin configuration unreadable: {e}")))?;
        let identity = orbit_types::workspace::git_remote_identity(&remote_url)
            .map_err(|e| refuse(e.to_string()))?;
        let repository = identity
            .strip_prefix("github.com/")
            .ok_or_else(|| refuse("source repair requires an exact GitHub origin identity"))?
            .to_string();
        if adopted.as_ref().is_some_and(|binding| {
            binding.repository != repository || binding.remote_url != remote_url
        }) {
            return Err(refuse("adopted source repository identity changed"));
        }
        let url = format!("https://github.com/{repository}/pull/{number}");
        task_pr_association(&task, number, &url)?;
        let status = self.read_pull_request_status(number)?;
        if status.url.as_deref() != Some(&url)
            || status.number.map(|n| n.to_string()).as_deref() != Some(number)
            || status.state.as_deref() != Some("OPEN")
            || status.head_branch.as_deref() != Some(branch)
            || status.head_sha.as_deref() != Some(head)
            || status.base_branch.as_deref() != Some(base_branch)
            || status.merge_commit.is_some()
            || status.merged_at.is_some()
            || status.auto_merge_enabled != Some(false)
        {
            return Err(refuse(
                "provider PR identity changed, closed/merged, or automatic merge intent is uncertain; inspect it explicitly",
            ));
        }
        // A draft is preserved as draft. Its evidence hold is never released here.
        if field(pr, "pr_url")? != url {
            return Err(refuse("checkpoint PR repository identity disagrees"));
        }
        let binding = SourceRepairBinding {
            schema_version: 1,
            workspace_id: self.workspace_id()?,
            task_id: task_id.into(),
            source_run_id: source_id.into(),
            machine_id: local.to_string(),
            source_digest: digest(
                &json!({"run":source,"steps":source.steps,"checkpoints":state,"definition":definition}),
            )?,
            task_digest: task_digest(&task)?,
            repository,
            remote_url,
            branch: branch.into(),
            pull_request: number.into(),
            pull_request_url: url,
            head_sha: head.into(),
            base_sha: base_sha.into(),
            base_branch: base_branch.into(),
        };
        Ok((source, task, binding))
    }
}
