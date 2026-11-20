//! Reuse the output certified by an atomic pilot apply, across consumers.

use super::super::TaskUpdateParams;
use crate::OrbitRuntime;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry};

use serde_json::{Value, json};
use std::collections::BTreeSet;

const PILOT_REQUIREMENTS_EDITED_EVENT: &str = "pilot_requirements_edited";
/// History marker of a `context_files` edit by someone other than the pilot or
/// Orbit, naming the pilot receipt it follows. A pilot warning or hold clears
/// automatically when its latest receipt has one; run-time widenings, the
/// pilot's own selector writes, plans, summaries and comments write none.
pub(super) const PILOT_CONTEXT_FILES_EDITED_EVENT: &str = "pilot_context_files_edited";
/// Orbit's own writes, which never ask for a fresh pilot.
const SYSTEM_WRITER: &str = "system";

impl OrbitRuntime {
    /// Return the latest applied assessment for automatic scheduling. An
    /// assessment remains evidence that a pilot ran even after material edits;
    /// it does not certify the edited material for dispatch.
    pub(crate) fn task_pilot_scheduling_assessment(
        &self,
        task_id: &str,
        resolved_comments: &[TaskComment],
    ) -> Result<Option<bool>, OrbitError> {
        self.task_pilot_assessment_from_comments(task_id, resolved_comments, false)
    }

    /// Reuse applied evidence for preparation unless edited requirements need
    /// another pilot. Unresolved pilots keep their holds; the preparation host
    /// withholds live execution claims before consulting this evidence.
    /// Unrelated material or consumer-policy changes never queue a repilot.
    pub(crate) fn task_pilot_preparation_assessment(
        &self,
        task_id: &str,
    ) -> Result<Option<bool>, OrbitError> {
        let comments = self.get_task_pilot_comments(task_id)?;
        self.task_pilot_assessment_from_comments(task_id, &comments, true)
    }

    /// `comments` must carry resolved pilot audits.
    fn task_pilot_assessment_from_comments(
        &self,
        task_id: &str,
        comments: &[TaskComment],
        reassess_requirements: bool,
    ) -> Result<Option<bool>, OrbitError> {
        for (_, receipt) in super::pilot_receipts(comments) {
            if !super::is_operation_id(receipt.operation_id) {
                continue;
            }
            // Only the latest accepted receipt is relevant. In particular,
            // never fall back to an older ready verdict after a new finding.
            // An unreadable body fails closed rather than skipping to an
            // older receipt, unlike blocker warnings.
            let Ok(audit) = serde_json::from_str::<Value>(receipt.body) else {
                return Ok(None);
            };
            let Some(assessment) = audit.get("assessment").filter(|value| value.is_object()) else {
                return Ok(None);
            };
            if reassess_requirements
                && pilot_requirements_stale(
                    &self.get_task(task_id)?,
                    &audit,
                    receipt.operation_id,
                    &|| self.get_task_history(task_id),
                )?
            {
                let history = self.get_task_history(task_id)?;
                let latest_applied = history
                    .iter()
                    .rev()
                    .find(|entry| entry.event == "task_pilot_applied")
                    .map(|entry| entry.at);
                let unresolved = history.iter().any(|entry| {
                    entry.event == "task_pilot_unresolved"
                        && latest_applied.is_none_or(|applied| entry.at > applied)
                });
                if !unresolved {
                    return Ok(None);
                }
            }
            return Ok(Some(super::member_ready(assessment)));
        }
        Ok(None)
    }
}

pub(super) fn pilot_requirements_stale(
    task: &Task,
    audit: &Value,
    operation_id: &str,
    history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
) -> Result<bool, OrbitError> {
    if let Some(sealed) = audit["assessed_requirements"].as_str() {
        return Ok(sealed != pilot_requirements_material(task));
    }
    // Pre-seal atomic receipts carry the same per-field hashes. Compare only
    // recorded requirements, preserving historical receipt compatibility.
    for (key, current) in [
        ("description", json!(task.description.trim())),
        ("criteria", json!(task.acceptance_criteria)),
    ] {
        if let Some(sealed) = audit["applied_material"]["material_components"][key].as_str() {
            let digest = orbit_automation::delivery::definition_epoch(&current)
                .map_err(orbit_automation::automation_error_to_orbit)?;
            if sealed != digest {
                return Ok(true);
            }
        }
    }
    if ["description", "criteria"].iter().any(|key| {
        audit["applied_material"]["material_components"][key]
            .as_str()
            .is_none()
    }) {
        return Ok(history()?.iter().any(|entry| {
            entry.event == PILOT_REQUIREMENTS_EDITED_EVENT
                && entry
                    .note
                    .as_deref()
                    .and_then(|note| serde_json::from_str::<Value>(note).ok())
                    .is_some_and(|record| {
                        record["pilot_operation_id"].as_str() == Some(operation_id)
                    })
        }));
    }
    Ok(false)
}

impl OrbitRuntime {
    /// Preserve unchanged pre-seal receipts, while recording future edits to
    /// requirements they did not seal. The task update persists this marker
    /// atomically with its edit; a later assessment has its own receipt id.
    pub(crate) fn pilot_requirement_edit_history(
        &self,
        task: &Task,
        params: &TaskUpdateParams,
        actor: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let description_changed = params
            .description
            .as_ref()
            .is_some_and(|value| value.trim() != task.description.trim());
        let criteria_changed = params
            .acceptance_criteria
            .as_ref()
            .is_some_and(|value| *value != task.acceptance_criteria);
        if !description_changed && !criteria_changed {
            return Ok(None);
        }
        let comments = self.get_task_pilot_comments(&task.id)?;
        let Some((_, receipt)) = super::pilot_receipts(&comments).next() else {
            return Ok(None);
        };
        let audit = serde_json::from_str::<Value>(receipt.body).unwrap_or_default();
        if audit["assessed_requirements"].is_string() {
            return Ok(None);
        }
        let unknown_description = description_changed
            && audit["applied_material"]["material_components"]["description"]
                .as_str()
                .is_none();
        let unknown_criteria = criteria_changed
            && audit["applied_material"]["material_components"]["criteria"]
                .as_str()
                .is_none();
        if !unknown_description && !unknown_criteria {
            return Ok(None);
        }
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: actor.to_string(),
            event: PILOT_REQUIREMENTS_EDITED_EVENT.into(),
            note: Some(json!({"pilot_operation_id": receipt.operation_id}).to_string()),
            operation_id: None,
            from_status: None,
            to_status: None,
            decision: None,
        }))
    }
}

/// Whether `history` records a `context_files` edit that followed the pilot
/// receipt `operation_id`.
pub(super) fn pilot_context_files_edited(history: &[TaskHistoryEntry], operation_id: &str) -> bool {
    history.iter().any(|entry| {
        entry.event == PILOT_CONTEXT_FILES_EDITED_EVENT
            && entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<Value>(note).ok())
                .is_some_and(|record| record["pilot_operation_id"].as_str() == Some(operation_id))
    })
}

impl OrbitRuntime {
    /// Record an ordinary writer's change to `context_files` against the
    /// task's latest pilot receipt, so the next preparation pass re-pilots a
    /// held task. The pilot's and Orbit's own writes, and writes that leave the
    /// selector set unchanged, record nothing. Persisted atomically with the
    /// task update.
    pub(crate) fn pilot_context_files_edit_history(
        &self,
        task: &Task,
        params: &TaskUpdateParams,
        actor: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let Some(next) = params.context_files.as_ref() else {
            return Ok(None);
        };
        let actor = actor.trim();
        if actor == super::PILOT_ACTOR || actor == SYSTEM_WRITER {
            return Ok(None);
        }
        let current: BTreeSet<&String> = task.context_files.iter().collect();
        if next.iter().collect::<BTreeSet<_>>() == current {
            return Ok(None);
        }
        let comments = self.get_task_comments(&task.id)?;
        let Some((_, receipt)) = super::pilot_receipts(&comments).next() else {
            return Ok(None);
        };
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: actor.to_string(),
            event: PILOT_CONTEXT_FILES_EDITED_EVENT.into(),
            note: Some(json!({"pilot_operation_id": receipt.operation_id}).to_string()),
            operation_id: None,
            from_status: None,
            to_status: None,
            decision: None,
        }))
    }
}

pub(super) fn pilot_requirements_material(task: &Task) -> String {
    sha256_hex(
        json!([task.description.trim(), task.acceptance_criteria])
            .to_string()
            .as_bytes(),
    )
}
