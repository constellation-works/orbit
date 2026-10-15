//! Continue only the candidate a durable operator reservation admitted.
use super::super::git::git_output;
use super::super::pr::read_pr_status;
use super::adopt::{Adoption, adopt_published_branch};
use super::apply::{Applied, apply, output, record};
use super::{Candidate, Outcome};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::{JobRun, SOURCE_REPAIR_RESERVED, SourceRepairAdmission};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn continue_source<H: RuntimeHost + ?Sized>(
    host: &H,
    run: &JobRun,
    task: &Task,
    request: &Value,
    workspace: &Path,
    base: &str,
) -> Result<Value, OrbitError> {
    let refused = |reason: &str| OrbitError::Execution(format!("source repair refused: {reason}"));
    let admission: SourceRepairAdmission =
        serde_json::from_value(request.clone()).map_err(|_| refused("unreadable admission"))?;
    let binding = &admission.binding;
    host.validate_source_repair(&run.run_id)?;
    if run.retry_source_run_id.as_deref() != Some(binding.source_run_id.as_str())
        || task.id != binding.task_id
        || task.job_run_id.as_deref() != Some(run.run_id.as_str())
        || run
            .input
            .as_ref()
            .is_none_or(|input| input["completion"] != "review" || input["auto_fix"] != false)
        || host.local_machine_id().as_deref() != Some(binding.machine_id.as_str())
    {
        return Err(refused("run/task/scope/authority binding changed"));
    }
    let reserved = host
        .get_task_history(&task.id)?
        .iter()
        .filter(|entry| entry.event == SOURCE_REPAIR_RESERVED && entry.by == "system")
        .filter_map(|entry| entry.note.as_deref())
        .any(|note| serde_json::from_str::<Value>(note).is_ok_and(|value| value == *request));
    if !reserved {
        return Err(refused("durable operator reservation missing"));
    }
    let origin = git_output(workspace, &["config", "--get", "remote.origin.url"])?;
    if origin != binding.remote_url {
        return Err(refused("repository remote changed"));
    }
    let remote = git_output(
        workspace,
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{}", binding.branch),
        ],
    )?;
    if remote.split_whitespace().next() != Some(binding.head_sha.as_str()) {
        return Err(refused("published branch lease changed"));
    }
    let pr = read_pr_status(host, workspace, &binding.pull_request)?;
    if pr.url.as_deref() != Some(binding.pull_request_url.as_str())
        || pr.state.as_deref() != Some("OPEN")
        || pr.number.map(|n| n.to_string()).as_deref() != Some(binding.pull_request.as_str())
        || pr.head_sha.as_deref() != Some(binding.head_sha.as_str())
        || pr.head_branch.as_deref() != Some(binding.branch.as_str())
        || pr.base_branch.as_deref() != Some(binding.base_branch.as_str())
        || pr.auto_merge_enabled != Some(false)
        || pr.merge_commit.is_some()
        || pr.merged_at.is_some()
    {
        return Err(refused("PR identity or merge intent changed"));
    }
    let candidate = Candidate {
        run_id: binding.source_run_id.clone(),
        branch: binding.branch.clone(),
        head_sha: binding.head_sha.clone(),
        durable_ref: None,
        failed_step_id: "operator_source_repair".into(),
        needs_review_repair: false,
        held: false,
        claim: None,
        published: true,
        pull_request: Some(binding.pull_request.clone()),
        pull_request_head: Some(binding.branch.clone()),
        interrupted_review: None,
    };
    // A refusal never falls back to a new branch/PR or a fresh implementation.
    match adopt_published_branch(workspace, &task.id, &candidate)? {
        Adoption::Adopted => {}
        Adoption::Refused(reason) => return Err(refused(&reason)),
    }
    let mut repair = json!({"trigger":"continuation","failed_step_id":"operator_source_repair","output":admission.diagnosis});
    match apply(&candidate, &candidate.head_sha, workspace, base)? {
        Applied::Refused(reason) => return Err(refused(&reason)),
        Applied::Conflict { paths, output } => {
            repair["trigger"] = json!("conflict");
            repair["conflicting_paths"] = json!(paths);
            repair["output"] = json!(format!("{}\n\n{output}", admission.diagnosis));
        }
        Applied::Clean | Applied::AlreadyPresent => {}
    }
    let outcome = Outcome::Repair(repair);
    record(host, task, &run.run_id, &candidate, &outcome)?;
    let mut result = output(&outcome, Some(&candidate), base);
    result["prior_pull_request"] = json!(binding.pull_request);
    result["prior_pull_request_head"] = json!(binding.branch);
    result["reused_branch"] = json!(binding.branch);
    result["reused_head_sha"] = json!(binding.head_sha);
    Ok(result)
}

/// A host-authenticated completed owner candidate skips implementation after a
/// clean apply. All later validation, review and completion gates run afresh.
pub(super) fn continue_published_owner<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    task: &Task,
    binding: &orbit_types::workflow::SourceRepairBinding,
    workspace: &Path,
    base: &str,
) -> Result<Value, OrbitError> {
    let refused =
        |reason: &str| OrbitError::Execution(format!("published candidate refused: {reason}"));
    let run = host
        .get_job_run(run_id)?
        .ok_or_else(|| refused("admitting run missing"))?;
    if task.job_run_id.as_deref() != Some(run_id)
        || binding.task_id != task.id
        || host.local_machine_id().as_deref() != Some(binding.machine_id.as_str())
        || run
            .executed_on
            .as_ref()
            .is_none_or(|o| o.machine_id != binding.machine_id)
        || run.job_id != "task_pr_pipeline"
        || run
            .input
            .as_ref()
            .is_none_or(|input| input["claimed"] == true || input["task_ids"] != json!([task.id]))
        || !matches!(task.status, orbit_types::task::TaskStatus::InProgress)
        || !matches!(run.state, orbit_types::workflow::JobRunState::Running)
    {
        return Err(refused("admitting execution binding changed"));
    }
    if git_output(workspace, &["config", "--get", "remote.origin.url"])? != binding.remote_url {
        return Err(refused("repository changed"));
    }
    let pr = read_pr_status(host, workspace, &binding.pull_request)?;
    if !exact_open_pr(binding, &pr, &binding.head_sha) {
        return Err(refused(
            "provider PR or merge outcome changed; reconcile explicitly",
        ));
    }
    let candidate = Candidate {
        run_id: binding.source_run_id.clone(),
        branch: binding.branch.clone(),
        head_sha: binding.head_sha.clone(),
        durable_ref: None,
        failed_step_id: "complete_pr".into(),
        needs_review_repair: false,
        held: false,
        claim: None,
        published: true,
        pull_request: Some(binding.pull_request.clone()),
        pull_request_head: Some(binding.branch.clone()),
        interrupted_review: None,
    };
    let outcome = match apply(&candidate, &candidate.head_sha, workspace, base)? {
        Applied::Clean => Outcome::Carried,
        Applied::Conflict { paths, output } => {
            Outcome::Repair(json!({"trigger":"conflict","conflicting_paths":paths,"output":output}))
        }
        Applied::AlreadyPresent => {
            return Err(refused(
                "published changes are already on the base; reconcile the existing PR",
            ));
        }
        Applied::Refused(reason) => return Err(refused(&reason)),
    };
    match adopt_published_branch(workspace, &task.id, &candidate)? {
        Adoption::Adopted => {}
        Adoption::Refused(reason) => return Err(refused(&reason)),
    }
    record(host, task, run_id, &candidate, &outcome)?;
    let mut result = output(&outcome, Some(&candidate), base);
    result["published_binding"] =
        serde_json::to_value(binding).map_err(|e| refused(&e.to_string()))?;
    result["reused_branch"] = json!(binding.branch);
    result["reused_head_sha"] = json!(binding.head_sha);
    result["prior_pull_request"] = json!(binding.pull_request);
    result["prior_pull_request_head"] = json!(binding.branch);
    Ok(result)
}

/// Publication reads the immutable run input, not a caller-shaped candidate
/// checkpoint. The owner pipeline and claimed pipeline pass different fields.
pub(in crate::executor::automation::vcs) fn publication_admission<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
    let Some(id) = input
        .get("run_id")
        .or_else(|| input.get("job_run_id"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let Some(admission) = host.source_repair_admission(id)? else {
        return host.owner_publication_binding(id);
    };
    let run = host
        .get_job_run(id)?
        .ok_or_else(|| OrbitError::Execution("source repair publication run missing".into()))?;
    let input = run
        .input
        .as_ref()
        .ok_or_else(|| OrbitError::Execution("source repair publication input missing".into()))?;
    if admission.binding.schema_version != 1
        || run
            .executed_on
            .as_ref()
            .is_none_or(|origin| origin.machine_id != admission.binding.machine_id)
        || !matches!(
            run.state,
            orbit_types::workflow::JobRunState::Pending
                | orbit_types::workflow::JobRunState::Running
        )
        || run.retry_source_run_id.as_deref() != Some(admission.binding.source_run_id.as_str())
        || host.local_machine_id().as_deref() != Some(admission.binding.machine_id.as_str())
        || input["completion"] != "review"
        || input["auto_fix"] != false
    {
        return Err(OrbitError::Execution(
            "source repair publication authority changed".into(),
        ));
    }
    Ok(Some(admission.binding))
}

/// A before-landing reviewer fix follows this run's own successful initial
/// publication. Its explicit lease cannot authorize an unrelated caller head.
pub(in crate::executor::automation::vcs) fn publication_lease<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    binding: &orbit_types::workflow::SourceRepairBinding,
) -> Result<String, OrbitError> {
    let Some(lease) = input.get("lease_remote_sha").and_then(Value::as_str) else {
        return Ok(binding.head_sha.clone());
    };
    let refused = || {
        OrbitError::Execution(
            "push_lease_lost: source repair review lease lacks this run's successful publication"
                .into(),
        )
    };
    let id = input
        .get("run_id")
        .or_else(|| input.get("job_run_id"))
        .and_then(Value::as_str)
        .ok_or_else(refused)?;
    let state = host.read_run_state(id)?.ok_or_else(refused)?;
    let published = state.pipeline.get("push").ok_or_else(refused)?;
    if state.run_id != id
        || state.job_id != "task_pr_pipeline"
        || published["phase"] != "push"
        || published["branch"] != binding.branch
        || published["local_sha"] != lease
        || !state.step_outputs.iter().any(|(index, output)| {
            output == published
                && state.step_states.get(index)
                    == Some(&orbit_types::workflow::JobRunState::Success)
        })
    {
        return Err(refused());
    }
    Ok(lease.into())
}

pub(in crate::executor::automation::vcs) fn exact_open_pr(
    binding: &orbit_types::workflow::SourceRepairBinding,
    pr: &super::super::pr::PullRequestStatus,
    expected_head: &str,
) -> bool {
    pr.number.map(|n| n.to_string()).as_deref() == Some(binding.pull_request.as_str())
        && pr.url.as_deref() == Some(binding.pull_request_url.as_str())
        && pr.state.as_deref() == Some("OPEN")
        && pr.head_branch.as_deref() == Some(binding.branch.as_str())
        && pr.head_sha.as_deref() == Some(expected_head)
        && pr.base_branch.as_deref() == Some(binding.base_branch.as_str())
        && pr.auto_merge_enabled == Some(false)
        && pr.merge_commit.is_none()
        && pr.merged_at.is_none()
}
