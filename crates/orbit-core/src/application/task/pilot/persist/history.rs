//! History summary of an applied assessment and material-change detail.

use std::collections::BTreeSet;

use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;

use super::super::apply::admission::PreparedTaskSnapshot;

/// Only applied no-target assessments carry this marker. The status is kept
/// separately because the shared material hash treats proposed and backlog
/// as equally eligible under the default predicate.
pub(in super::super) fn no_target_assessment_marker(note: &str) -> Option<(TaskStatus, &str)> {
    let (_, marker) = note.split_once(" [no-target-assessed:")?;
    let (marker, _) = marker.split_once(']')?;
    let (status, fingerprint) = marker.split_once(':')?;
    if !super::super::is_operation_id(fingerprint) {
        return None;
    }
    Some((status.parse().ok()?, fingerprint))
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
