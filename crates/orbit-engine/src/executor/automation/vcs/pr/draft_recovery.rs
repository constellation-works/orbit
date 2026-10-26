//! Recover only drafts whose live body identifies an Orbit section gap.

use orbit_common::{OrbitError, security::release::sha256_hex};
use orbit_types::{
    task::{TaskDecision, TaskHistoryEntry},
    workflow::{PR_DRAFT_RECOVERY_EVENT, PrDraftRecovery, PrDraftRecoveryOutcome},
};
use serde_json::json;

use super::super::{handoff::HandoffContext, operations};
use super::{answers::PullRequestStatus, body::MISSING_SECTION_NOTE};
use crate::context::{PrConfig, RuntimeHost, TaskAutomationUpdate};

pub(super) fn recover_section_gap_draft<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    pr: &PullRequestStatus,
    body: &str,
    missing: &[String],
    config: &PrConfig,
) -> Result<(), OrbitError> {
    if !pr.is_draft {
        return Ok(());
    }
    let number = pr.number_selector.as_deref().ok_or_else(|| {
        OrbitError::Execution("draft recovery requires the pull request number".into())
    })?;
    let url = pr.url.as_deref().ok_or_else(|| {
        OrbitError::Execution("draft recovery requires the pull request URL".into())
    })?;
    let live_body = pr.body.as_deref().unwrap_or_default();
    let mut reserved = false;
    // A body edit can succeed before ready fails or the executor is interrupted.
    // Only an exact, durable body reservation authorizes that retry; human edits
    // invalidate it. The terminal Ready record ends the reservation.
    for task in &context.tasks {
        let last = host
            .get_task_history(&task.id)?
            .into_iter()
            .rev()
            .find_map(|entry| {
                if entry.by != "system" || entry.event != PR_DRAFT_RECOVERY_EVENT {
                    return None;
                }
                match entry.decision {
                    Some(TaskDecision::PrDraftRecovery(record)) if record.pr_url == url => {
                        Some(record)
                    }
                    _ => None,
                }
            });
        reserved |= last.is_some_and(|record| {
            record.outcome != PrDraftRecoveryOutcome::Ready
                && record.body_sha256.as_deref() == Some(sha256_hex(live_body.as_bytes()).as_str())
        });
    }
    let outcome = if config.conventions.draft {
        PrDraftRecoveryOutcome::ConfiguredDraft
    } else if !live_body.contains(MISSING_SECTION_NOTE) && !reserved {
        PrDraftRecoveryOutcome::UnrelatedDraft
    } else if !missing.is_empty() {
        PrDraftRecoveryOutcome::MissingSections
    } else {
        PrDraftRecoveryOutcome::RefreshingBody
    };
    let mut record = PrDraftRecovery {
        pr_url: url.into(),
        outcome,
        body_sha256: reserved.then(|| sha256_hex(live_body.as_bytes())),
    };
    if record.outcome != PrDraftRecoveryOutcome::RefreshingBody {
        return record_draft_recovery(host, context, &record);
    }
    record.body_sha256 = Some(sha256_hex(body.as_bytes()));
    record_draft_recovery(host, context, &record)?;
    host.run_private_vcs_operation(
        operations::PR_EDIT_BODY,
        json!({"pr": number, "workspace_path": context.workspace_path, "body": body}),
    )?;
    host.run_private_vcs_operation(
        operations::PR_READY,
        json!({"pr": number, "workspace_path": context.workspace_path}),
    )?;
    record.outcome = PrDraftRecoveryOutcome::Ready;
    record_draft_recovery(host, context, &record)
}

fn record_draft_recovery<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    record: &PrDraftRecovery,
) -> Result<(), OrbitError> {
    let reason = match record.outcome {
        PrDraftRecoveryOutcome::ConfiguredDraft => "kept draft: pr.draft is configured",
        PrDraftRecoveryOutcome::UnrelatedDraft => "kept draft: no section-gap recovery evidence",
        PrDraftRecoveryOutcome::MissingSections => {
            "kept draft: required sections are still missing"
        }
        PrDraftRecoveryOutcome::RefreshingBody => {
            "refreshing body: required sections are now filled"
        }
        PrDraftRecoveryOutcome::Ready => {
            "marked ready for review: required sections are now filled"
        }
    };
    for task in &context.tasks {
        host.apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".into(),
                    event: PR_DRAFT_RECOVERY_EVENT.into(),
                    note: Some(format!("{}: {reason}.", record.pr_url)),
                    decision: Some(TaskDecision::PrDraftRecovery(record.clone())),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                }],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}
