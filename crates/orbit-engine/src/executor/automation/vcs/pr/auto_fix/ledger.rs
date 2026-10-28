use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::workflow::PrAutoFixBlocker;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::push::record_published_pr_head;
use super::super::super::retry_lineage::{LineageStep, LineageStop, RetryLineage};
use super::{admission, verdict};

/// A resume has a new execution identity while successful push checkpoints
/// retain the source head. Only the host's durable, same-job retry ancestry
/// transfers repair authority; input/checkpoint claims do not supply it.
/// Direct activity calls without a stored run retain current-run authority.
pub(super) fn repair_ledger_runs<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
) -> Result<BTreeSet<String>, OrbitError> {
    let mut runs = BTreeSet::from([run_id.to_string()]);
    let Some(current) = host.get_job_run(run_id)? else {
        return Ok(runs);
    };
    if current.run_id != run_id {
        return Err(OrbitError::Execution(
            "auto_fix: execution identity differs; refusing inherited repair authority".into(),
        ));
    }
    let mut lineage = RetryLineage::same_job(&current).checking_identity();
    let refusal = loop {
        match lineage.step(host)? {
            LineageStep::Ancestor(parent_id, _) => {
                if !runs.insert(parent_id) {
                    break "cyclic retry lineage";
                }
            }
            LineageStep::Stop(LineageStop::Root) => return Ok(runs),
            LineageStep::Stop(LineageStop::Cycle) => break "cyclic retry lineage",
            LineageStep::Stop(LineageStop::Missing(_)) => break "retry ancestor is missing",
            LineageStep::Stop(LineageStop::CrossJob { .. } | LineageStop::IdentityMismatch) => {
                break "retry ancestor identity differs";
            }
            LineageStep::Stop(LineageStop::Bound) => break "retry lineage exceeds its bound",
        }
    };
    Err(OrbitError::Execution(format!(
        "auto_fix: {refusal}; refusing inherited repair authority"
    )))
}

/// Retain the failed and passing exact-head evidence before refreshing.
pub(in crate::executor::automation::vcs::pr) fn record_landed_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    evidence: &Value,
) -> Result<(), OrbitError> {
    let head = evidence["failed_head"]
        .as_str()
        .ok_or_else(|| verdict::stopped("", "missing failed head"))?;
    retain(
        host,
        auto_fix,
        &format!("auto-fix/{}/landed-{head}.json", auto_fix.run_id),
        evidence,
    )?;
    record_base_path(host, auto_fix, "pr_auto_fix_base_refresh", evidence)
}

/// Reserve a base refresh separately from agent repair attempts. A previous
/// reservation never authorizes another rewrite, including after interruption.
pub(in crate::executor::automation::vcs::pr) fn reserve_base_refresh<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    candidate: &orbit_types::workflow::handoff::HandoffCandidate,
    deadline_ms: u64,
    evidence: &Value,
) -> Result<(), OrbitError> {
    use orbit_types::workflow::{PrBaseUpdateAdmission, PrBaseUpdateReservation};
    let request = PrBaseUpdateReservation {
        execution: auto_fix.run_id.clone(),
        repository: candidate.repository.clone(),
        pr_number: match candidate.delivery {
            orbit_types::workflow::handoff::HandoffDelivery::PullRequest { number } => {
                number.to_string()
            }
            _ => return Err(verdict::stopped("", "base refresh requires a pull request")),
        },
        head_sha: evidence["failed_head"].as_str().unwrap_or_default().into(),
        head_branch: candidate.source_branch.clone(),
        base_branch: candidate.base_branch.clone(),
        base_sha: evidence["landed_base"].as_str().unwrap_or_default().into(),
        deadline_ms,
        evidence: json!({"cause":"auto_fix_landed", "checks":evidence}),
    };
    match host.admit_pr_base_update(&auto_fix.task_ids, &request)? {
        PrBaseUpdateAdmission::Reserved => Ok(()),
        PrBaseUpdateAdmission::Observe(_) => Err(verdict::stopped(
            &request.pr_number,
            "the base refresh was already reserved; no second branch rewrite is allowed",
        )),
        PrBaseUpdateAdmission::Refused(why) => Err(verdict::stopped(&request.pr_number, &why)),
    }
}

/// Record a base decision once per exact evidence on every owning task;
/// repeated known-red polls do not flood history.
pub(in crate::executor::automation::vcs::pr) fn record_base_path<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    event: &str,
    evidence: &Value,
) -> Result<(), OrbitError> {
    let note = serde_json::to_string(evidence)
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
    for task in &auto_fix.task_ids {
        if host
            .get_task_history(task)?
            .iter()
            .any(|entry| entry.event == event && entry.note.as_deref() == Some(&note))
        {
            continue;
        }
        host.apply_task_automation_update(
            task,
            TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".into(),
                    event: event.into(),
                    note: Some(note.clone()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}

/// Retain the authenticated check annotations a repair of `head` relies on,
/// before it is admitted. A repeated read of the same head replaces them.
pub(super) fn record_check_annotations<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    pr_number: &str,
    head: &str,
    annotations: Vec<Value>,
) -> Result<(), OrbitError> {
    retain(
        host,
        auto_fix,
        &format!("auto-fix/{}/check-annotations-{head}.json", auto_fix.run_id),
        &json!({"pr": pr_number, "failed_head": head, "checks": annotations}),
    )
}

/// Attach `evidence`, redacted, to the owning run of the primary task at
/// `path`. A task with no owning run stops with `missing_owner_reason`.
pub(in crate::executor::automation::vcs::pr) fn attach_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    pr_number: &str,
    missing_owner_reason: &str,
    path: &str,
    evidence: &Value,
) -> Result<(), OrbitError> {
    let primary = auto_fix.primary_task()?;
    let owner_run = host
        .get_task(primary)?
        .job_run_id
        .ok_or_else(|| verdict::stopped(pr_number, missing_owner_reason))?;
    let evidence = orbit_common::security::redaction::redact_all_json(evidence.clone());
    host.attach_task_validation_log(
        primary,
        &owner_run,
        path,
        serde_json::to_vec(&evidence).map_err(|error| OrbitError::Execution(error.to_string()))?,
    )
}

fn retain<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    path: &str,
    evidence: &Value,
) -> Result<(), OrbitError> {
    attach_evidence(host, auto_fix, "", "missing owning run", path, evidence)
}

/// Record a head this run published on the pull request.
pub(in crate::executor::automation::vcs::pr) fn record_published_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    pr_number: &str,
    from_head_sha: &str,
    to_head_sha: &str,
    blocker: PrAutoFixBlocker,
) -> Result<(), OrbitError> {
    record_published_pr_head(
        host,
        auto_fix.primary_task()?,
        &auto_fix.run_id,
        pr_number,
        from_head_sha,
        to_head_sha,
        blocker,
    )
}
