use std::collections::BTreeSet;

use orbit_store::contracts::OwnerWaveCandidate;
use orbit_types::task::NO_DIFF_EXPECTED_TAG;
use orbit_types::workflow::DrainWaitingTask;

use crate::OrbitRuntime;
use crate::application::admission::auto_admission::AdmissionSelection;
use crate::application::admission::backlog_exclusion::{BacklogSnapshot, BacklogTaskExclusion};
use crate::application::admission::claim_holders::ClaimHolders;
use crate::application::admission::pass_error::PassError;
use crate::runtime::task::locks::lock_context_files_for_task;

/// How many excluded tasks one pass records; the total is recorded beside the
/// list so a truncated one still reads as truncated.
const EXCLUDED_BACKLOG_RECORDED: usize = 20;

/// An exclusion's reason code, as the drain records it.
pub(super) fn waiting_reason(
    reason: crate::application::admission::backlog_exclusion::BacklogTaskExclusionReason,
) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "excluded".to_string())
}

pub(super) fn waiting_excluded(
    excluded: &[BacklogTaskExclusion],
    claim_holders: &ClaimHolders,
) -> Vec<DrainWaitingTask> {
    excluded
        .iter()
        .take(EXCLUDED_BACKLOG_RECORDED)
        .map(|entry| {
            let blocked_by = entry
                .conflicts
                .iter()
                .map(|conflict| conflict.locking_task_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            // [ORB-15514] The claim behind a lock wait, beside whatever else
            // the exclusion already says.
            let claims = claim_holders.detail_for(
                blocked_by
                    .iter()
                    .map(String::as_str)
                    .filter(|blocker| *blocker != entry.id),
            );
            DrainWaitingTask {
                task_id: entry.id.clone(),
                reason: Some(waiting_reason(entry.reason)),
                blocked_by,
                detail: match (entry.detail.clone(), claims) {
                    (Some(detail), Some(claims)) => Some(format!("{detail} {claims}")),
                    (detail, claims) => detail.or(claims),
                },
                findings: Vec::new(),
            }
        })
        .collect()
}

/// [ORB-15514] Pass the wave through the pull admission section. The wave
/// was chosen from a snapshot and its gates reserve only later, so without
/// this a pull grant could take an admitted footprint in between, or have
/// claimed one just before the snapshot that missed it. The section refuses
/// a candidate a live claim holds and makes every other admitted footprint
/// visible to pull grants before it ends.
pub(super) fn admit_through_section(
    runtime: &OrbitRuntime,
    action: &str,
    selection: &mut AdmissionSelection,
    snapshot: &BacklogSnapshot,
) -> Result<(), PassError> {
    let task_lookup = &snapshot.task_lookup;
    let workspace_root = runtime.paths().repo_root.as_path();
    let candidates = selection
        .selected
        .iter()
        .filter_map(|task_id| task_lookup.get(task_id))
        .map(|task| OwnerWaveCandidate {
            task_id: task.id.clone(),
            // [ORB-14247] No-diff work holds no lock, so it excludes no
            // overlapping pull either; the row still keeps the task itself.
            // Nor does a resumed published delivery, which takes its locks
            // only when it first edits.
            footprint: if task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
                || snapshot.published_resumes.contains_key(&task.id)
            {
                Vec::new()
            } else {
                lock_context_files_for_task(task, workspace_root, runtime.unlocked_context_paths())
            },
        })
        .collect::<Vec<_>>();
    let admissions = runtime
        .stores()
        .tasks()
        .admit_owner_wave(&candidates)
        .map_err(|error| {
            PassError::store(
                action,
                "admit the wave through the admission section",
                error,
            )
        })?;
    for admission in admissions {
        if let (Some(claim), Some(task)) =
            (admission.blocked_by, task_lookup.get(&admission.task_id))
        {
            selection.refuse_claimed(
                task,
                &claim,
                workspace_root,
                runtime.unlocked_context_paths(),
            );
        }
    }
    Ok(())
}
