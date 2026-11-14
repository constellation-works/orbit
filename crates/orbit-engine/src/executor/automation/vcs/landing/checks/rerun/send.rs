//! Sending a reserved failed-jobs retry, and the retry history it records.

use super::super::super::super::operations;
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{
    CI_CHECK_RERUN_EVENT, CI_INFRASTRUCTURE_RETRY_BACKOFF_EVENT, CiCheckRerunEvent,
    CiCheckRerunJob, CiCheckRerunRecord, CiInfrastructureRetryBackoffRecord, CiRerunAdmission,
    CiRerunPlan,
};
use serde_json::json;
use std::path::Path;

/// Send a just-reserved failed-jobs retry once and record it. `false` when
/// the reservation or the owner's authority changed before the send.
pub(super) fn send_reserved_rerun<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    handoff: Option<&str>,
    pr: u64,
    plan: &CiRerunPlan,
    deadline_ms: u64,
) -> Result<bool, OrbitError> {
    // The exact set and budget are durable now. A lost/error reply is
    // ambiguous; never retransmit. The next poll reads actual checks.
    if !matches!(host.admit_ci_rerun(tasks,handoff,plan,super::super::super::super::completion_deadline::now_ms()?)?,CiRerunAdmission::Observe(previous) if *previous==*plan)
    {
        return Ok(false);
    }
    let run_id = plan.run_id;
    let reply=host.run_private_vcs_operation(operations::CI_RERUN_FAILED,json!({"workspace_path":workspace,"run_id":run_id.to_string(),"plan":plan,"deadline_ms":deadline_ms}));
    if reply.is_ok() {
        let jobs = plan
            .roots
            .iter()
            .map(|root| {
                let original = plan
                    .failed_jobs
                    .iter()
                    .find(|job| job["id"].as_u64() == Some(root.job_id));
                let time =
                    |key: &str| original.and_then(|job| job[key].as_str().map(str::to_string));
                CiCheckRerunJob {
                    job_id: Some(root.job_id),
                    check: Some(root.check.clone()),
                    classification: Some(root.classification.clone()),
                    signature: Some(root.signature.clone()),
                    run_attempt: Some(plan.run_attempt),
                    started_at: time("started_at"),
                    completed_at: time("completed_at"),
                }
            })
            .collect();
        let note = CiCheckRerunRecord {
            head: Some(plan.head_sha.clone()),
            pr_number: Some(pr),
            run_id: Some(plan.run_id),
            event: Some(CiCheckRerunEvent::PullRequest),
            jobs,
            legacy_pull_requests: Vec::new(),
        }
        .to_history_note()
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if let Some(handoff) = handoff {
            host.record_handoff_landing_history(handoff, CI_CHECK_RERUN_EVENT, &note)?;
        } else {
            for task in tasks {
                host.apply_task_automation_update(
                    task,
                    crate::context::TaskAutomationUpdate {
                        append_history: vec![orbit_types::task::TaskHistoryEntry {
                            at: chrono::Utc::now(),
                            by: "system".into(),
                            event: CI_CHECK_RERUN_EVENT.into(),
                            note: Some(note.clone()),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )?;
            }
        }
    }
    Ok(true)
}

pub(super) fn record_cancellation_exhausted<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    handoff: Option<&str>,
    reason: &str,
) -> Result<(), OrbitError> {
    if let Some(handoff) = handoff {
        host.record_handoff_landing_history(handoff, "ci_check_rerun_exhausted", reason)?;
        return Ok(());
    }
    for task in tasks {
        host.apply_task_automation_update(
            task,
            crate::context::TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".into(),
                    event: "ci_check_rerun_exhausted".into(),
                    note: Some(reason.to_string()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// Read only the durable ledger while backing off; do not re-fetch completed
/// jobs, logs or provider authentication on every completion poll.
pub(in crate::executor::automation::vcs::landing::checks) fn infrastructure_backoff<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    tasks: &[String],
    head: &str,
    pr: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(task) = tasks.iter().min() else {
        return Ok(None);
    };
    let history = host.get_task_history(task)?;
    let now = super::super::super::super::completion_deadline::now_ms()?;
    for entry in history
        .iter()
        .rev()
        .filter(|entry| entry.event == CI_INFRASTRUCTURE_RETRY_BACKOFF_EVENT)
    {
        let record = CiInfrastructureRetryBackoffRecord::from_history_note(entry.note.as_deref())
            .map_err(|error| {
            OrbitError::Execution(format!("unreadable CI retry backoff: {error}"))
        })?;
        if record.head.value().map(String::as_str) == Some(head)
            && record.pr_number.value().map(String::as_str) == Some(pr)
        {
            let not_before = record.not_before_ms.value().copied().ok_or_else(|| {
                OrbitError::Execution("CI retry backoff has no not-before".into())
            })?;
            return Ok((now < not_before)
                .then(|| format!("infrastructure CI retry backs off until {not_before} ms")));
        }
    }
    Ok(None)
}

pub(super) fn record_retry_exhausted<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    handoff: Option<&str>,
    plan: &CiRerunPlan,
    attempts: u32,
    reason: &str,
) -> Result<(), OrbitError> {
    let event = "ci_infrastructure_retry_exhausted";
    let note = json!({"head":plan.head_sha,"pr_number":plan.pr_number,
        "classification":"transient","attempts":attempts,"reason":reason})
    .to_string();
    record_retry_history(host, tasks, handoff, event, &note)
}

pub(super) fn record_retry_history<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    handoff: Option<&str>,
    event: &str,
    note: &str,
) -> Result<(), OrbitError> {
    if let Some(handoff) = handoff {
        host.record_handoff_landing_history(handoff, event, note)?;
    } else {
        for task in tasks {
            if host
                .get_task_history(task)?
                .iter()
                .any(|entry| entry.event == event && entry.note.as_deref() == Some(note))
            {
                continue;
            }
            host.apply_task_automation_update(
                task,
                crate::context::TaskAutomationUpdate {
                    append_history: vec![orbit_types::task::TaskHistoryEntry {
                        at: chrono::Utc::now(),
                        by: "system".into(),
                        event: event.into(),
                        note: Some(note.to_string()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )?;
        }
    }
    Ok(())
}
