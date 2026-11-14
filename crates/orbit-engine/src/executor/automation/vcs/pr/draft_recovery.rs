//! Recover only drafts whose section-gap evidence names the live body.

use orbit_common::{OrbitError, security::release::sha256_hex};
use orbit_types::{
    task::{TaskDecision, TaskHistoryEntry},
    workflow::{
        MissingSectionPolicy, PR_DRAFT_RECOVERY_EVENT, PR_SECTION_GAP_EVENT, PrDraftRecovery,
        PrDraftRecoveryOutcome, PrSectionGap,
    },
};
use serde_json::json;

use super::super::{handoff::HandoffContext, operations};
use super::{answers::PullRequestStatus, body};
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
    } else if !reserved
        && draft_section_gap(host, context.tasks.iter().map(|task| task.id.as_str()), pr)?.is_none()
    {
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

/// Identify the section repair a handed-off draft needs. The repair leaf's
/// resumed `pr_open` rebuilds the body and uses `recover_section_gap_draft`;
/// landing itself never invents missing content or marks a draft ready.
pub(in crate::executor::automation::vcs) fn handed_off_section_gaps<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &std::path::Path,
    task_id: &str,
    pr_number: &str,
) -> Result<Vec<String>, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_VIEW,
        json!({"pr": pr_number, "workspace_path": workspace}),
    )?;
    let pr = PullRequestStatus::from_view_response(&response)?;
    if !pr.is_draft {
        return Ok(Vec::new());
    }
    Ok(draft_section_gap(host, std::iter::once(task_id), &pr)?
        .map(|gap| gap.missing_sections)
        .unwrap_or_default())
}

/// The section-gap decision that published this draft's live body, read once
/// from the tasks' history. Precedence:
///
/// 1. A typed [`PrSectionGap`] whose publication is this head branch and the
///    exact live body wins.
/// 2. A typed publication from this head branch with another body means the
///    body changed after Orbit published it (a human edit): no evidence, and
///    the body's prose cannot override it.
/// 3. Only without any typed publication for the branch, the legacy reader
///    recognizes Orbit's missing-section note in a body published before the
///    typed record existed.
pub(in crate::executor::automation::vcs) fn draft_section_gap<'a, H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: impl Iterator<Item = &'a str>,
    pr: &PullRequestStatus,
) -> Result<Option<PrSectionGap>, OrbitError> {
    let live_body = pr.body.as_deref().unwrap_or_default();
    let digest = sha256_hex(live_body.as_bytes());
    let head = pr.head_branch.as_deref();
    let mut published_from_head = false;
    let mut matching = None;
    for task_id in task_ids {
        for entry in host.get_task_history(task_id)? {
            let Some(TaskDecision::PrSectionGap(gap)) = entry.decision else {
                continue;
            };
            if entry.by != "system" || entry.event != PR_SECTION_GAP_EVENT {
                continue;
            }
            let from_head = match head {
                Some(head) => gap.published_from(head),
                None => gap.publication.is_some(),
            };
            if !from_head {
                continue;
            }
            published_from_head = true;
            if head.is_some_and(|head| gap.published_body(head, &digest)) {
                matching = Some(gap);
            }
        }
    }
    if matching.is_some() || published_from_head {
        return Ok(matching);
    }
    Ok(legacy_section_gap(live_body, head))
}

/// The isolated legacy reader: a body published before [`PrSectionGap`] was
/// recorded identifies its gaps only by Orbit's missing-section note under
/// each required heading. It yields the same view the typed record does,
/// without execution or publication identity.
fn legacy_section_gap(live_body: &str, head: Option<&str>) -> Option<PrSectionGap> {
    let missing_sections = body::missing_section_names(live_body);
    (!missing_sections.is_empty()).then(|| PrSectionGap {
        missing_sections,
        action: MissingSectionPolicy::Draft,
        run_id: String::new(),
        head_branch: head.unwrap_or_default().to_string(),
        publication: None,
    })
}

fn record_draft_recovery<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    record: &PrDraftRecovery,
) -> Result<(), OrbitError> {
    let reason = match record.outcome {
        PrDraftRecoveryOutcome::ConfiguredDraft => {
            "kept draft: the delivery conventions open every pull request as a draft"
        }
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
                append_history: vec![
                    TaskHistoryEntry::new(chrono::Utc::now(), "system", PR_DRAFT_RECOVERY_EVENT)
                        .with_note(Some(format!("{}: {reason}.", record.pr_url)))
                        .with_decision(Some(TaskDecision::PrDraftRecovery(record.clone()))),
                ],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}
