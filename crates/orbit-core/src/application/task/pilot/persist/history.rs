//! Applied assessment receipts, history summaries and material-change detail.

use std::collections::BTreeSet;

use orbit_types::task::{Task, TaskComment, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::{TaskAutomationPort, legacy_decisions};

use super::super::apply::admission::PreparedTaskSnapshot;

/// Only applied no-target assessments carry this receipt. The status is kept
/// separately because the shared material hash treats proposed and backlog
/// as equally eligible under the default predicate.
#[derive(Deserialize, Serialize)]
pub(in super::super) struct NoTargetPilotAssessment {
    pub(super) status: TaskStatus,
    pub(super) fingerprint: String,
}

/// Only the targetless assessment is typed here; the remaining audit body
/// retains its existing shape and interpretation.
#[derive(Deserialize, Serialize)]
pub(super) struct NoTargetPilotReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) no_target_assessment: Option<NoTargetPilotAssessment>,
    #[serde(flatten)]
    pub(super) audit: Value,
}

/// Decode only the receipt belonging to the latest applied history event.
/// A valid assessment's presence is sufficient, just as the old marker was:
/// this does not compare its status or fingerprint with current material.
pub(in super::super) fn no_target_pilot_assessment(
    applied: &TaskHistoryEntry,
    comments: &[TaskComment],
) -> Option<NoTargetPilotAssessment> {
    // As in atomic::applied_material, the first receipt sealed by the
    // operation wins over later comments that happen to reuse its id.
    if let Some(operation_id) = applied.receipt()
        && let Some((_, receipt)) = super::super::pilot_receipts(comments)
            .rev()
            .find(|(_, receipt)| receipt.operation_id == operation_id)
    {
        let audit: NoTargetPilotReceipt = serde_json::from_str(receipt.body).ok()?;
        if let Some(assessment) = audit.no_target_assessment {
            return super::super::is_operation_id(&assessment.fingerprint).then_some(assessment);
        }
    }
    let (status, fingerprint) =
        legacy_decisions::no_target_pilot_assessment_marker(applied.note.as_deref()?)?;
    Some(NoTargetPilotAssessment {
        status,
        fingerprint: fingerprint.to_string(),
    })
}

pub(super) fn assessment_history_summary(assessment: &Value) -> String {
    let disposition = assessment["disposition"].as_str().unwrap_or("unknown");
    let confidence = assessment["confidence"].as_str().unwrap_or("unknown");
    let rationale = assessment["assessment_rationale"]
        .as_str()
        .unwrap_or_default()
        .split(['.', '!', '?'])
        .next()
        .unwrap_or_default();
    let rationale = rationale
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let rationale = rationale.split_whitespace().collect::<Vec<_>>().join(" ");
    let short = rationale.chars().take(64).collect::<String>();
    let truncated = rationale.chars().count() > 64;
    format!(
        "{disposition} (confidence {confidence}) — {short}{}",
        if truncated { "…" } else { "" }
    )
}

pub(super) fn status_only_fingerprint(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
) -> Option<String> {
    let (_, revision) = snapshot.material.as_ref()?;
    let neutral = snapshot.status_neutral_fingerprint.as_ref()?;
    let fresh = runtime
        .task_pilot_fingerprints(current, revision, policy)
        .ok()?;
    if &fresh.status_neutral != neutral {
        return None;
    }
    Some(fresh.material)
}

pub(super) const MATERIAL_CHANGED_DETAIL: &str =
    "task meaning or dependency evidence changed after preparation";

/// Name the freshness components whose digests moved. An empty diff stays
/// unnamed: eligibility can change the material hash without any configured
/// component moving, and that is not a status change either. A missing
/// component map is an older prepared payload and keeps the same sentence.
pub(super) fn material_change_detail(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
) -> String {
    let Some(stored) = snapshot.material_components.as_ref() else {
        return MATERIAL_CHANGED_DETAIL.to_string();
    };
    let Some((_, revision)) = snapshot.material.as_ref() else {
        return MATERIAL_CHANGED_DETAIL.to_string();
    };
    let Ok(fresh) = runtime.task_freshness_digests(current, revision, policy) else {
        return MATERIAL_CHANGED_DETAIL.to_string();
    };
    let mut drifted = BTreeSet::new();
    for (key, expected) in stored {
        if fresh.get(key) != Some(expected) {
            drifted.insert(key.as_str());
        }
    }
    for key in fresh.keys() {
        if !stored.contains_key(key) {
            drifted.insert(key.as_str());
        }
    }
    if drifted.is_empty() {
        return MATERIAL_CHANGED_DETAIL.to_string();
    }
    format!(
        "{MATERIAL_CHANGED_DETAIL}: {}",
        drifted.into_iter().collect::<Vec<_>>().join(", ")
    )
}
