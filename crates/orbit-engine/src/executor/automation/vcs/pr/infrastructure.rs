//! Completion's generic prerequisite recovery; the adapter owns forge evidence.
use super::super::{freshness::remote_branch_sha, operations};
use super::{
    answers::{PullRequestStatus, read_required_checks},
    auto_fix::{AutoFix, stopped},
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{
    PrInfrastructureAdmission, PrInfrastructureEvidence, pr_infrastructure_reservations,
};
use serde_json::{Value, json};
use std::path::Path;

pub(super) enum InfrastructureStep {
    None,
    Wait,
}

fn record<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    evidence: &PrInfrastructureEvidence,
    suffix: &str,
    value: &Value,
) -> Result<(), OrbitError> {
    let primary = auto_fix.primary_task()?;
    let owner = host.get_task(primary)?.job_run_id.ok_or_else(|| {
        stopped(
            &evidence.pr_number,
            "infrastructure evidence has no owning run",
        )
    })?;
    let path = format!(
        "auto-fix/{}/infrastructure-{}-{suffix}.json",
        auto_fix.run_id, evidence.run_id
    );
    let redacted = orbit_common::security::redaction::redact_all_json(value.clone());
    host.attach_task_validation_log(
        primary,
        &owner,
        &path,
        serde_json::to_vec(&redacted).map_err(|error| OrbitError::Execution(error.to_string()))?,
    )
}
fn exact_remote(
    workspace: &str,
    status: &PullRequestStatus,
    evidence: &PrInfrastructureEvidence,
) -> Result<(), OrbitError> {
    if status.head_sha.as_deref() != Some(&evidence.head_sha)
        || status.head_branch.as_deref() != Some(&evidence.head_branch)
        || status.base_branch.as_deref() != Some(&evidence.base_branch)
        || status.number.map(|n| n.to_string()).as_deref() != Some(&evidence.pr_number)
        || status.url.as_deref()
            != Some(&format!(
                "https://github.com/{}/pull/{}",
                evidence.repository, evidence.pr_number
            ))
    {
        return Err(stopped(
            &evidence.pr_number,
            "infrastructure PR identity changed",
        ));
    }
    if remote_branch_sha(Path::new(workspace), &evidence.head_branch)?.as_deref()
        != Some(&evidence.head_sha)
    {
        return Err(stopped(
            &evidence.pr_number,
            "infrastructure remote head changed or was deleted",
        ));
    }
    Ok(())
}

/// Observe a durable reservation on every poll, even CLEAN. On a fresh
/// optional-only failure, obtain complete typed evidence before admission.
/// A lost mutation reply spends the reservation; only observation follows.
pub(super) fn recover<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    workspace: &str,
    pr: &str,
    status: &PullRequestStatus,
    diagnose: bool,
    remaining_seconds: u64,
) -> Result<InfrastructureStep, OrbitError> {
    let deadline = std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_secs(remaining_seconds))
        .ok_or_else(|| stopped(pr, "completion deadline overflow"))?;
    let history = host.get_task_history(auto_fix.primary_task()?)?;
    let reservations =
        pr_infrastructure_reservations(&history).map_err(|reason| stopped(pr, &reason))?;
    if let Some(reservation) = reservations
        .iter()
        .find(|reservation| reservation.evidence.pr_number == pr)
    {
        let evidence = &reservation.evidence;
        if remaining_seconds == 0 {
            return Err(stopped(
                pr,
                "completion deadline expired with a reserved infrastructure rerun",
            ));
        }
        exact_remote(workspace, status, evidence)?;
        let observed = host
            .run_private_vcs_operation(
                operations::CI_PREREQUISITE_OBSERVE,
                json!({"workspace_path":workspace,"evidence":evidence}),
            )
            .map_err(|error| stopped(pr, &error.to_string()))?;
        record(host, auto_fix, evidence, "observation", &observed)?;
        if std::time::SystemTime::now() >= deadline {
            return Err(stopped(
                pr,
                "completion deadline expired while observing infrastructure recovery",
            ));
        }
        if observed["gates_passed"] == true {
            return Ok(InfrastructureStep::None);
        }
        return Ok(InfrastructureStep::Wait);
    }
    if !diagnose {
        return Ok(InfrastructureStep::None);
    }
    let head = status
        .head_sha
        .as_deref()
        .ok_or_else(|| stopped(pr, "missing prerequisite head"))?;
    let policy = read_required_checks(host, Path::new(workspace), pr).map_err(|error| {
        stopped(
            pr,
            &format!("its required checks could not be read: {error}"),
        )
    })?;
    if policy.head_sha != head {
        return Err(stopped(pr, "required checks name another head"));
    }
    // Existing required failures retain the ordinary code-repair path.
    if status.status_checks.iter().flatten().any(|check| {
        check.state == Some(super::answers::CheckState::Failed)
            && check
                .name
                .as_ref()
                .is_some_and(|name| policy.requires(check.workflow_name.as_deref(), name))
    }) {
        return Ok(InfrastructureStep::None);
    }
    if remaining_seconds == 0 {
        return Err(stopped(
            pr,
            "no completion deadline remains for infrastructure recovery",
        ));
    }
    let response = host.run_private_vcs_operation(operations::CI_PREREQUISITE_EVIDENCE, json!({"workspace_path":workspace,"pr":pr,"head_sha":head,"head_branch":status.head_branch,"base_branch":status.base_branch})).map_err(|error| stopped(pr, &error.to_string()))?;
    if response.get("evidence") == Some(&Value::Null) {
        return Ok(InfrastructureStep::None);
    }
    let evidence: PrInfrastructureEvidence =
        serde_json::from_value(response["evidence"].clone())
            .map_err(|error| stopped(pr, &format!("unreadable prerequisite evidence: {error}")))?;
    exact_remote(workspace, status, &evidence)?;
    if std::time::SystemTime::now() >= deadline {
        return Err(stopped(
            pr,
            "completion deadline expired while reading infrastructure evidence",
        ));
    }
    let deadline_ms = u64::try_from(
        deadline
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| stopped(pr, &error.to_string()))?
            .as_millis(),
    )
    .map_err(|_| stopped(pr, "completion deadline overflow"))?;
    let admission =
        host.admit_pr_infrastructure(&auto_fix.run_id, &auto_fix.task_ids, &evidence)?;
    match admission {
        PrInfrastructureAdmission::Refused(reason) => Err(stopped(pr, &reason)),
        PrInfrastructureAdmission::Observe(reservation) => {
            exact_remote(workspace, status, &reservation.evidence)?;
            host.run_private_vcs_operation(
                operations::CI_PREREQUISITE_OBSERVE,
                json!({"workspace_path":workspace,"evidence":reservation.evidence}),
            )
            .map_err(|error| stopped(pr, &error.to_string()))?;
            Ok(InfrastructureStep::Wait)
        }
        PrInfrastructureAdmission::Reserved(reservation) => {
            record(
                host,
                auto_fix,
                &reservation.evidence,
                "original",
                &json!(reservation),
            )?;
            exact_remote(workspace, status, &evidence)?;
            // The adapter rechecks policy, latest attempt and source evidence
            // immediately before this single-shot mutation. Never retry it.
            let reply = host.run_private_vcs_operation(operations::CI_RERUN_FAILED, json!({"workspace_path":workspace,"run_id":evidence.run_id.to_string(),"prerequisite":evidence,"deadline_ms":deadline_ms}));
            let permanent_refusal = reply
                .as_ref()
                .ok()
                .filter(|reply| reply["refusal"] == "credential_or_permission")
                .map(|reply| {
                    reply["diagnostic"]
                        .as_str()
                        .unwrap_or("workflow rerun credentials or permission were refused")
                        .to_string()
                });
            record(
                host,
                auto_fix,
                &evidence,
                "reply",
                &match reply {
                    Ok(reply) => json!({"reply":reply}),
                    Err(error) => json!({"unknown_reply":error.to_string()}),
                },
            )?;
            if let Some(reason) = permanent_refusal {
                return Err(stopped(pr, &reason));
            }
            // A failure is not proof that the write was unapplied. Reconcile
            // latest attempt before making any further decision.
            let observed = host
                .run_private_vcs_operation(
                    operations::CI_PREREQUISITE_OBSERVE,
                    json!({"workspace_path":workspace,"evidence":evidence}),
                )
                .map_err(|error| stopped(pr, &error.to_string()))?;
            record(host, auto_fix, &evidence, "observation", &observed)?;
            Ok(InfrastructureStep::Wait)
        }
    }
}
