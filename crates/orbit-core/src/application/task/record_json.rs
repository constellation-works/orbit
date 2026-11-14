//! The full task record JSON every read surface serves.

use std::collections::BTreeMap;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    Task, TaskStatus, serialize_task_artifacts, show_external_refs_json,
    task_artifact_manifest_json, task_base_json, task_show_record_field_json,
    task_show_reference_field_json, unknown_task_show_field_message,
};
use serde_json::{Map, Value};

use super::{TaskOsReadout, TaskRow};
use crate::OrbitRuntime;
use crate::application::task::{TaskExperimentPort, TaskReviewPort};

impl OrbitRuntime {
    /// `task` with its comment, history and artifact sidecars read from this
    /// runtime's stores.
    pub fn task_row_with_sidecars(&self, task: Task) -> Result<TaskRow, OrbitError> {
        let comments = self.get_task_comments(&task.id)?;
        let history = self.get_task_history(&task.id)?;
        let artifacts = self.get_task_artifact_manifest(&task.id)?;
        Ok(TaskRow {
            task,
            comments,
            history,
            artifacts,
        })
    }
}

/// The full task record: the shared base ([`task_base_json`]), the
/// `comments`, `history` and `artifacts` sidecars, the settled before-PR
/// `review` gate when one exists [ORB-11333], and the host-local crew
/// enrichment ([`crate::runtime::RuntimeKernel::task_crew_read`]).
///
/// A single-task show serves [`task_show_json`], which is this record plus
/// `block`, `next_step` and `final_recovery`. Writes and list rows stay on this record.
pub fn task_record_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Map<String, Value>, OrbitError> {
    let task = &row.task;
    let mut object = task_base_json(task, status_by_id);
    if let Some(at) = orbit_types::task::task_started_at(&row.history) {
        object.insert("started_at".into(), serde_json::json!(at));
    }
    if let Some(at) = task.not_before {
        object.insert(
            "scheduled_hold".into(),
            serde_json::json!(
                task.status == TaskStatus::Backlog
                    && at > chrono::Utc::now()
                    && !orbit_types::task::schedule_overridden(&row.history, at)
            ),
        );
    }
    TaskOsReadout::new(runtime)?.insert_into(task, &mut object);
    // Read projections must tolerate corrupt pilot receipts: the CLI contract
    // keeps malformed task-pilot comments as ordinary text (see orbit-cli
    // `pilot_comment_rendering`). A strict decode failure therefore means "no
    // resolvable hold", not a failed record. Other errors still propagate.
    let resolvable_pilot_admission_hold = if task.status.is_terminal() {
        false
    } else {
        match runtime.pilot_admission_hold_in(task, &row.comments, &|| Ok(row.history.clone())) {
            Ok(hold) => hold.is_some_and(|hold| hold.is_resolvable()),
            Err(OrbitError::Execution(message))
                if message.starts_with("decode ")
                    || message.contains("pilot receipt")
                    || message.contains("pilot assessment") =>
            {
                false
            }
            // Missing, unreadable or invalid assessment evidence cannot
            // resolve a hold in this read projection. Admission stays strict.
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Artifact,
                ..
            })
            | Err(OrbitError::Io(_))
            | Err(OrbitError::InvalidInput(_)) => false,
            // A dangling or tampered artifact blob fails the full bundle
            // read that resolves pilot evidence, even though the task's
            // structural metadata (envelope, sidecars, manifest) is
            // readable through the lightweight reads that built this row.
            // Degrade only that payload failure; any other bundle damage
            // keeps propagating below.
            Err(error) if pilot_artifact_payload_corruption(&error) => false,
            Err(error) => return Err(error),
        }
    };
    object.insert(
        "resolvable_pilot_admission_hold".into(),
        serde_json::json!(resolvable_pilot_admission_hold),
    );
    object.insert(
        "pilot_warnings".into(),
        serde_json::json!(
            runtime.task_pilot_warning_findings(task, &row.comments, &|id: &str| task_is_done(
                runtime,
                Some(status_by_id),
                id
            ),)
        ),
    );
    object.insert(
        "comments".to_string(),
        serde_json::to_value(&row.comments).map_err(serialize_error("comments"))?,
    );
    object.insert(
        "history".to_string(),
        serde_json::to_value(&row.history).map_err(serialize_error("history"))?,
    );
    object.insert(
        "artifacts".to_string(),
        task_artifact_manifest_json(&row.artifacts),
    );
    if let Some(review) = runtime.task_review_evidence(task, &row.artifacts)? {
        object.insert("review".to_string(), review);
    }
    runtime.task_crew_read(task).insert_into(&mut object);
    Ok(object)
}

/// The show record: [`task_record_json`] plus the operator guidance only a
/// single-task readout carries.
///
/// `block` explains the latest block or escalation (reason, time and source).
/// `next_step` says what to do about a task a failed run left `blocked`.
/// `final_recovery` is the last automated final-recovery decision, with
/// `run_id`, `decision`, `outcome`, `fault` (`unknown` on an older decision),
/// `fault_evidence`, `fault_judged_by` and `at`. `completion_hold` is the
/// `completion.hold` rule and match that kept a completion-authorized run's
/// task in `review`, while it is still there. `owed_criteria` lists each
/// criterion the delivery owes after deploy, with its host, command, target
/// and state. Each key is present only when it applies. `orbit task show`, `orbit.task.show`, `GET /api/tasks/:id` and a
/// routed remote show all serve this record and add only their own extras.
pub fn task_show_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Map<String, Value>, OrbitError> {
    let mut object = task_record_json(runtime, row, status_by_id)?;
    // The show projection reports the task's current delivery: the latest
    // recorded `github-pr` first. Storage appends, so without this a requeued
    // task that opened a fresh pull request would keep reporting the closed
    // earlier one (the CLI's `PR:` line reads the first match).
    object.insert(
        "external_refs".to_string(),
        show_external_refs_json(&row.task),
    );
    object.insert(
        "experiments".into(),
        serde_json::to_value(runtime.task_experiment_assignment_readout(&row.task.id)?)
            .map_err(serialize_error("experiments"))?,
    );
    if let Some(block) = super::blocked_recovery::block_json(row) {
        object.insert("block".to_string(), block);
    }
    if let Some(next) = runtime.blocked_next_step(&row.task, &row.history) {
        object.insert(
            "next_step".to_string(),
            serde_json::to_value(next).map_err(serialize_error("next_step"))?,
        );
    }
    if let Some(record) = super::FinalRecoveryRecord::last(&row.history, &row.comments) {
        object.insert("final_recovery".to_string(), final_recovery_json(&record));
    }
    if let Some(hold) =
        orbit_types::workflow::current_completion_hold(row.task.status, &row.history)
    {
        object.insert(
            "completion_hold".to_string(),
            serde_json::to_value(hold).map_err(serialize_error("completion_hold"))?,
        );
    }
    if let Some(owed) = super::owed_criteria_json(&row.history) {
        object.insert("owed_criteria".to_string(), owed);
    }
    if row
        .history
        .iter()
        .any(|entry| entry.event == orbit_types::task::DECISION_REQUESTED_EVENT)
    {
        let open = runtime.open_decisions_json_in(&row.task, &row.history, &row.comments)?;
        if open.as_array().is_some_and(|items| !items.is_empty()) {
            object.insert("open_decisions".to_string(), open);
        }
    }
    Ok(object)
}

/// The last final-recovery decision as `task show` and the `final_recovery`
/// projection serve it. It names no crew or model, so a fault audit can read
/// it blind to the implementer's model.
fn final_recovery_json(record: &super::FinalRecoveryRecord) -> Value {
    serde_json::json!({
        "run_id": record.run_id,
        "decision": record.decision,
        "outcome": record.outcome,
        "fault": record.fault,
        "fault_evidence": record.fault_evidence,
        "fault_judged_by": record.fault_judged_by,
        "at": record.at,
    })
}

/// One `orbit.task.show` / `orbit task show --fields` projection.
///
/// A record field answers the value [`task_base_json`] carries for it; a
/// sidecar is read on demand, with `artifacts` in its bounded metadata form;
/// the crew enrichments answer `null` where their case does not apply.
/// `resolved_dependencies` and `relations` need `status_by_id`, which a caller
/// may leave out when it selects neither.
pub fn task_show_field_json(
    runtime: &OrbitRuntime,
    task: &Task,
    field: &str,
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
) -> Result<Value, OrbitError> {
    match field {
        "scheduled_hold" => {
            let held = if task.status == TaskStatus::Backlog
                && let Some(at) = task.scheduled_after(chrono::Utc::now())
            {
                !orbit_types::task::schedule_overridden(&runtime.get_task_history(&task.id)?, at)
            } else {
                false
            };
            Ok(serde_json::json!(held))
        }
        "started_at" => Ok(serde_json::json!(orbit_types::task::task_started_at(
            &runtime.get_task_history(&task.id)?
        ))),
        "experiments" => {
            serde_json::to_value(runtime.task_experiment_assignment_readout(&task.id)?)
                .map_err(serialize_error("experiments"))
        }
        "pilot_warnings" => Ok(serde_json::json!(runtime.task_pilot_warning_findings(
            task,
            &runtime.get_task_comments(&task.id)?,
            &|id: &str| task_is_done(runtime, status_by_id, id),
        ))),
        "os_requirement" => Ok(TaskOsReadout::new(runtime)?.requirement(task)),
        "placement_wait" => Ok(TaskOsReadout::new(runtime)?
            .placement_wait(task)
            .map_or(Value::Null, Value::String)),
        "open_decisions" => runtime.open_decisions_json(&task.id),
        "final_recovery" => Ok(super::FinalRecoveryRecord::last(
            &runtime.get_task_history(&task.id)?,
            &runtime.get_task_comments(&task.id)?,
        )
        .map_or(Value::Null, |record| final_recovery_json(&record))),
        "completion_hold" => {
            let history = runtime.get_task_history(&task.id)?;
            orbit_types::workflow::current_completion_hold(task.status, &history)
                .map_or(Ok(Value::Null), |hold| {
                    serde_json::to_value(hold).map_err(serialize_error("completion_hold"))
                })
        }
        "comments" => serde_json::to_value(runtime.get_task_comments(&task.id)?)
            .map_err(serialize_error("comments")),
        "history" => serde_json::to_value(runtime.get_task_history(&task.id)?)
            .map_err(serialize_error("history")),
        "artifacts" => Ok(serialize_task_artifacts(
            &runtime.get_task_artifact_manifest(&task.id)?,
        )),
        "resolved_dependencies" | "relations" => {
            let status_by_id = status_by_id.ok_or_else(|| {
                OrbitError::Execution(format!("missing task status index for {field}"))
            })?;
            Ok(task_show_reference_field_json(task, field, status_by_id).unwrap_or(Value::Null))
        }
        // The show field projection reports the same current-delivery order
        // as the full show record.
        "external_refs" => Ok(show_external_refs_json(task)),
        other => task_show_record_field_json(task, other)
            .or_else(|| runtime.task_crew_field_json(task, other))
            .ok_or_else(|| OrbitError::InvalidInput(unknown_task_show_field_message(other))),
    }
}

/// Whether `error` is the store's artifact-payload verification failure:
/// a manifest entry whose blob is absent, or whose stored bytes no longer
/// match the manifest's size or sha256. Those are the failures
/// `validate_artifact_manifest_files` reports; manifest, envelope and
/// sidecar damage reports differently and stays explicit.
fn pilot_artifact_payload_corruption(error: &OrbitError) -> bool {
    let reason = match error {
        OrbitError::Store(message) => message.as_str(),
        OrbitError::TaskBundleCorrupt { reason, .. } => reason.as_str(),
        _ => return false,
    };
    reason.contains("artifact manifest references missing file")
        || reason.contains("artifact manifest size mismatch")
        || reason.contains("artifact manifest sha256 mismatch")
}

fn serialize_error(label: &'static str) -> impl FnOnce(serde_json::Error) -> OrbitError {
    move |error| OrbitError::Execution(format!("serialize task {label}: {error}"))
}

/// A list projection preserves the row and marks unreadable fields as unknown,
/// with a warning, rather than discarding every healthy task in the page.
pub(crate) fn task_listing_field_json(
    runtime: &OrbitRuntime,
    task: &Task,
    field: &str,
    warnings: &mut Vec<String>,
) -> Result<Value, OrbitError> {
    let result = super::task_fields_response_json(runtime, task, &[field.to_string()]);
    Ok(super::listing::task_bundle_scan_result(result, warnings)?.unwrap_or(Value::Null))
}

fn task_is_done(
    runtime: &OrbitRuntime,
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
    id: &str,
) -> bool {
    status_by_id
        .and_then(|index| index.get(id).copied())
        .map(|status| status == TaskStatus::Done)
        .unwrap_or_else(|| runtime.warning_prerequisite_is_done(id))
}
