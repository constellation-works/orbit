//! Snapshot and assessed-material drift checks.

use orbit_types::task::Task;
use orbit_types::workflow::automation::members::PreparationPolicy;

use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;

use super::super::apply::admission::{PreparedTaskSnapshot, ValidatedTask};
use super::history::{MATERIAL_CHANGED_DETAIL, material_change_detail, status_only_fingerprint};

pub(super) fn task_snapshot_drift(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
) -> Option<(&'static str, &'static str)> {
    if snapshot
        .material
        .as_ref()
        .is_some_and(|(expected, revision)| {
            runtime
                .task_material_fingerprint(current, revision, policy)
                .map_or(true, |fingerprint| &fingerprint != expected)
        })
    {
        Some(("material_changed", MATERIAL_CHANGED_DETAIL))
    } else if current.context_files != snapshot.context_files {
        Some((
            "context_files_changed",
            "task context_files changed after preparation",
        ))
    } else if current.status != snapshot.status {
        Some(("status_changed", "task status changed after preparation"))
    } else if current.complexity != snapshot.complexity {
        Some((
            "complexity_changed",
            "task complexity changed after preparation",
        ))
    } else if current.title != snapshot.title {
        Some(("title_changed", "task title changed after preparation"))
    } else if current.tags != snapshot.tags {
        Some(("tags_changed", "task tags changed after preparation"))
    } else {
        None
    }
}

/// Compare the task with the exact output certified by the pilot's locked
/// write, including its OS routing and crew redraw. Status is the caller's to
/// judge; dependency status moves retain the pilot's freshness tolerance.
pub(in super::super) fn assessed_material_drift(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    validated: &ValidatedTask,
    policy: &PreparationPolicy,
) -> Option<(&'static str, String)> {
    let Some(written) = &validated.applied_material else {
        return Some((
            "pilot_write_unverified",
            "the pilot audit does not certify its written material; prepare a new assessment"
                .to_string(),
        ));
    };
    if current.context_files != validated.after {
        return Some((
            "context_files_changed",
            "task context_files changed after the pilot wrote them".to_string(),
        ));
    }
    if current.complexity != Some(validated.complexity) {
        return Some((
            "complexity_changed",
            "task complexity changed after the pilot wrote it".to_string(),
        ));
    }
    if current.crew != written.crew || current.crew_source != written.crew_source {
        return Some((
            "crew_changed",
            "task crew or assignment provenance changed after the pilot wrote it".to_string(),
        ));
    }
    if current.relations != written.relations {
        return Some((
            "relations_changed",
            "task relations changed after the pilot wrote them".to_string(),
        ));
    }
    let mut snapshot = snapshot.clone();
    snapshot.context_files = validated.after.clone();
    snapshot.complexity = Some(validated.complexity);
    snapshot.tags = written.tags.clone();
    snapshot.material = written.material.clone();
    snapshot.status_neutral_fingerprint = written.status_neutral_fingerprint.clone();
    snapshot.material_components = written.material_components.clone();
    let (reason, detail) = task_snapshot_drift(runtime, current, &snapshot, policy)?;
    if reason != "material_changed" {
        return Some((reason, detail.to_string()));
    }
    let Some(fingerprint) = status_only_fingerprint(runtime, current, &snapshot, policy) else {
        return Some((
            reason,
            material_change_detail(runtime, current, &snapshot, policy),
        ));
    };
    let mut rebased = snapshot;
    rebased.material = rebased
        .material
        .map(|(_, revision)| (fingerprint, revision));
    task_snapshot_drift(runtime, current, &rebased, policy)
        .map(|(reason, detail)| (reason, detail.to_string()))
}
