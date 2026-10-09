//! Locked, retried, idempotent persistence of one validated task-pilot assessment.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::security::release::sha256_hex;
use orbit_common::{OrbitError, StorageLayer};
use orbit_store::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::automation::members::SUPERSEDED_BY_SOURCE;

use super::admission::{Admission, PreparedTaskSnapshot, ValidatedTask};

const STORAGE_APPLY_ATTEMPTS: usize = 3;

/// Stale reason when the task's durable context creation grant is no longer
/// the one preparation recorded.
const CONTEXT_CREATION_CHANGED: &str = "context_creation_changed";
const CONTEXT_CREATION_CHANGED_DETAIL: &str =
    "task context creation authorization changed after preparation";

pub(super) enum ApplyTaskOutcome {
    Applied(Option<String>),
    AlreadyApplied(Option<String>),
    Stale(&'static str, String),
    Superseded(Value),
}

pub(super) fn apply_task(
    runtime: &OrbitRuntime,
    snapshot: &PreparedTaskSnapshot,
    task: &ValidatedTask,
    prepared: &Value,
    policy: &PreparationPolicy,
) -> Result<ApplyTaskOutcome, OrbitError> {
    if !matches!(snapshot.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(ApplyTaskOutcome::Stale(
            "status_not_mutable",
            "task-pilot does not rewrite in-progress, review, or terminal work".to_string(),
        ));
    }
    let mut snapshot = snapshot.clone();
    for attempt in 0..=1 {
        let mut lock_ids = vec![task.task_id.clone()];
        lock_ids.extend(runtime.get_task(&task.task_id)?.dependencies());
        lock_ids.sort();
        lock_ids.dedup();
        let mut outcome = None;
        let mut retry_fingerprint = None;
        let mut operation = || {
            let claim = crate::application::automation::members::claim(runtime, prepared)?;
            // Re-checked under the task locks: the branch may have moved
            // under this task's material since apply's own check [ORB-14476].
            if let Some(claim) = &claim
                && let Some(detail) = crate::application::automation::members::stale_tasks(
                    runtime,
                    claim,
                    policy,
                    prepared,
                    std::slice::from_ref(&task.task_id),
                    &BTreeMap::from([(task.task_id.clone(), task.after.clone())]),
                )?
                .remove(&task.task_id)
            {
                outcome = Some(ApplyTaskOutcome::Superseded(source_superseded(
                    &task.task_id,
                    &detail,
                )));
                return Ok(());
            }
            let receipt = format!("operation_id={}", task.operation_id);
            if runtime
                .get_task_history(&task.task_id)?
                .iter()
                .any(|event| {
                    event.event == "task_pilot_applied"
                        && event.note.as_deref().is_some_and(|note| {
                            note.ends_with(&format!(" ({receipt})"))
                                || note.lines().next() == Some(receipt.as_str())
                        })
                })
            {
                outcome = Some(ApplyTaskOutcome::AlreadyApplied(resulting_fingerprint(
                    runtime,
                    &task.task_id,
                    &snapshot,
                    policy,
                )?));
                return Ok(());
            }
            let current = match runtime.get_task(&task.task_id) {
                Ok(current) => current,
                Err(OrbitError::NotFound { .. }) => {
                    outcome = Some(ApplyTaskOutcome::Stale(
                        "task_deleted",
                        "task no longer exists at the write boundary".to_string(),
                    ));
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if let Some(superseded) = superseded_task(
                runtime,
                &task.task_id,
                &snapshot,
                policy,
                matches!(&task.admission, Some(Admission::CiSweep(_))),
            )? {
                outcome = Some(ApplyTaskOutcome::Superseded(superseded));
                return Ok(());
            }
            if let Some(reason) = task_snapshot_drift(runtime, &current, &snapshot, policy) {
                if attempt == 0 && matches!(reason.0, "material_changed" | "status_changed") {
                    retry_fingerprint =
                        status_only_fingerprint(runtime, &current, &snapshot, policy)
                            .map(|fingerprint| (fingerprint, current.status));
                }
                if retry_fingerprint.is_none() {
                    let detail = if reason.0 == "material_changed" {
                        material_change_detail(runtime, &current, &snapshot, policy)
                    } else {
                        reason.1.to_string()
                    };
                    outcome = Some(ApplyTaskOutcome::Stale(reason.0, detail));
                }
                return Ok(());
            }
            if runtime.context_creation_state(&current)?.identity()
                != snapshot.context_creation_identity
            {
                outcome = Some(ApplyTaskOutcome::Stale(
                    CONTEXT_CREATION_CHANGED,
                    CONTEXT_CREATION_CHANGED_DETAIL.to_string(),
                ));
                return Ok(());
            }
            if attempt == 1 && !matches!(current.status, TaskStatus::Proposed | TaskStatus::Backlog)
            {
                outcome = Some(ApplyTaskOutcome::Stale(
                    "status_changed",
                    "task status changed after preparation; task-pilot does not rewrite active work"
                        .to_string(),
                ));
                return Ok(());
            }

            let target_status = if task.promote {
                TaskStatus::Backlog
            } else {
                snapshot.status
            };
            let mut history_summary = assessment_history_summary(&task.assessment);
            if task.after.is_empty() {
                // Record the meaning *after* the atomic write. A marker made
                // from the prepared task would immediately be stale when the
                // pilot changes its complexity or status.
                let mut assessed = current.clone();
                assessed.context_files = task.after.clone();
                assessed.complexity = Some(task.complexity);
                assessed.status = target_status;
                let fingerprint = crate::application::automation::preparation::pilot_fingerprint(
                    runtime,
                    &assessed,
                    snapshot
                        .material
                        .as_ref()
                        .map(|(_, revision)| revision.as_str()),
                    policy,
                )
                .map_err(orbit_automation::automation_error_to_orbit)?;
                history_summary.push_str(&format!(
                    " [no-target-assessed:{}:{fingerprint}]",
                    target_status.cli_name()
                ));
            }
            if let Some(marker) = &task.history_marker {
                history_summary.push_str(marker);
            }
            let requirements = super::validation_tools::ImplementationLane::resolve(runtime)
                .operator_requirements(&current);
            let mut assessed = current.clone();
            assessed.context_files = task.after.clone();
            let crew_redraw_history =
                runtime.rerate_task_crew(&mut assessed, Some(task.complexity))?;
            assessed.complexity = Some(task.complexity);
            let operator_hold = (!requirements.is_empty()).then(|| {
                crate::application::task::OperatorValidationHold::new(&assessed, requirements)
            });
            let host_operational_hold = (task.assessment["disposition"] == "host_operational")
                .then(|| {
                    crate::application::task::HostOperationalHold::new(
                        &assessed,
                        task.assessment["evidence"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                });
            // Recorded whatever the task's tags are now: admission compares it
            // with the tags each time, so adding the matching `os:` tag
            // satisfies it and removing that tag restores it.
            let native_os_hold = (!task.required_os.is_empty()).then(|| {
                crate::application::task::NativeOsHold::new(&assessed, task.required_os.clone())
            });
            let mutation_params = AtomicTaskMutationParams {
                actor: "task-pilot".to_string(),
                operation_id: task.operation_id.clone(),
                expected_context_files: snapshot.context_files.clone(),
                expected_status: snapshot.status,
                expected_complexity: snapshot.complexity,
                expected_crew: current.crew.clone(),
                expected_crew_source: current.crew_source.clone(),
                crew: assessed.crew.clone(),
                crew_source: assessed.crew_source.clone(),
                expected_context_creation: snapshot.context_creation_identity.clone(),
                context_files: task.after.clone(),
                status: target_status,
                complexity: task.complexity,
                event_type: "task_pilot_applied".to_string(),
                event_note: "task-pilot atomic application".to_string(),
                history_summary,
                append_history: operator_hold
                    .as_ref()
                    .map(|hold| hold.history(&task.operation_id))
                    .into_iter()
                    .chain(
                        host_operational_hold
                            .as_ref()
                            .map(|hold| hold.history(&task.operation_id)),
                    )
                    .chain(crew_redraw_history)
                    .collect(),
                audit_note: serde_json::to_string(&json!({
                    "assessment": task.assessment,
                    "operator_validation_hold": operator_hold,
                    "host_operational_hold": host_operational_hold,
                    "native_os_hold": native_os_hold,
                    "context_files_before": snapshot.context_files,
                    "complexity_before": snapshot.complexity,
                    "complexity_after": task.complexity,
                }))
                .map_err(|error| {
                    OrbitError::Execution(format!("serialize task-pilot audit: {error}"))
                })?,
            };
            let mutation = apply_atomic_with_retries(runtime, &task.task_id, &mutation_params)?;
            match mutation {
                AtomicTaskMutationOutcome::Applied => {
                    runtime.record_event(OrbitEvent::TaskUpdated {
                        id: task.task_id.clone(),
                    })?;
                    outcome = Some(ApplyTaskOutcome::Applied(resulting_fingerprint(
                        runtime,
                        &task.task_id,
                        &snapshot,
                        policy,
                    )?));
                }
                AtomicTaskMutationOutcome::AlreadyApplied => {
                    outcome = Some(ApplyTaskOutcome::AlreadyApplied(resulting_fingerprint(
                        runtime,
                        &task.task_id,
                        &snapshot,
                        policy,
                    )?));
                }
                AtomicTaskMutationOutcome::Stale => {
                    outcome = Some(ApplyTaskOutcome::Stale(
                        "write_boundary_changed",
                        "task changed between validation and the atomic write boundary".to_string(),
                    ));
                }
            }
            Ok(())
        };
        if let Err(error) = with_task_locks(runtime, &lock_ids, 0, &mut operation) {
            // Claim admission fences ordinary writers before the closure can
            // run. Re-read durable ownership instead of matching error prose.
            if let Some(outcome) = superseded_task(
                runtime,
                &task.task_id,
                &snapshot,
                policy,
                matches!(&task.admission, Some(Admission::CiSweep(_))),
            )? {
                return Ok(ApplyTaskOutcome::Superseded(outcome));
            }
            return Err(error);
        }
        if let Some((fingerprint, status)) = retry_fingerprint {
            // Admit this fresh fingerprint only once. The next pass releases
            // and reacquires the task/dependency locks, then reads them again.
            snapshot.material = snapshot
                .material
                .map(|(_, revision)| (fingerprint, revision));
            snapshot.status = status;
            continue;
        }
        return outcome
            .ok_or_else(|| OrbitError::Execution("task-pilot operation did not run".to_string()));
    }
    Err(OrbitError::Execution(
        "task-pilot status retry loop did not settle".to_string(),
    ))
}

/// Only applied no-target assessments carry this marker. The status is kept
/// separately because the shared material hash treats proposed and backlog
/// as equally eligible under the default predicate.
pub(super) fn no_target_assessment_marker(note: &str) -> Option<(TaskStatus, &str)> {
    let (_, marker) = note.split_once(" [no-target-assessed:")?;
    let (marker, _) = marker.split_once(']')?;
    let (status, fingerprint) = marker.split_once(':')?;
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((status.parse().ok()?, fingerprint))
}

fn assessment_history_summary(assessment: &Value) -> String {
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
    let rationale = rationale
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("drain-approval-held:", "drain approval held:");
    let short = rationale.chars().take(64).collect::<String>();
    let truncated = rationale.chars().count() > 64;
    format!(
        "{disposition} (confidence {confidence}) — {short}{}",
        if truncated { "…" } else { "" }
    )
}

fn status_only_fingerprint(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
) -> Option<String> {
    let (_, revision) = snapshot.material.as_ref()?;
    let neutral = snapshot.status_neutral_fingerprint.as_ref()?;
    let fresh = crate::application::automation::preparation::fingerprints(
        runtime, current, revision, policy,
    )
    .ok()?;
    if &fresh.status_neutral != neutral {
        return None;
    }
    Some(fresh.material)
}

const MATERIAL_CHANGED_DETAIL: &str =
    "task meaning or dependency evidence changed after preparation";

/// Name the freshness components whose digests moved. An empty diff stays
/// unnamed: eligibility can change the material hash without any configured
/// component moving, and that is not a status change either. A missing
/// component map is an older prepared payload and keeps the same sentence.
fn material_change_detail(
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
    let Ok(fresh) = crate::application::automation::preparation::component_digests(
        runtime, current, revision, policy,
    ) else {
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

fn apply_atomic_with_retries(
    runtime: &OrbitRuntime,
    task_id: &str,
    params: &AtomicTaskMutationParams,
) -> Result<AtomicTaskMutationOutcome, OrbitError> {
    let mut last_error = None;
    for attempt in 0..STORAGE_APPLY_ATTEMPTS {
        match runtime
            .stores()
            .tasks()
            .apply_atomic_task_mutation(task_id, params)
        {
            Ok(outcome) => return Ok(outcome),
            Err(error)
                if error.storage_layer() == Some(StorageLayer::Io)
                    && attempt + 1 < STORAGE_APPLY_ATTEMPTS =>
            {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        OrbitError::Execution("task-pilot storage retry loop did not run".to_string())
    }))
}

fn resulting_fingerprint(
    runtime: &OrbitRuntime,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
) -> Result<Option<String>, OrbitError> {
    let Some((_, revision)) = &snapshot.material else {
        return Ok(None);
    };
    let current = runtime.get_task(task_id)?;
    crate::application::automation::preparation::fingerprint(runtime, &current, revision, policy)
        .map(Some)
        .map_err(orbit_automation::automation_error_to_orbit)
}

pub(super) fn task_operation_id(prepared: &Value, task_id: &str, assessment: &Value) -> String {
    let mut identity = json!({
        "task_id": task_id,
        "source": prepared.get("source"),
        "prepared_tasks": prepared.get("tasks"),
        "assessment": assessment,
    });
    // Replay receipts predate insertion-ordered plugin JSON.
    identity.sort_all_objects();
    let encoded = serde_json::to_vec(&identity).unwrap_or_default();
    sha256_hex(&encoded)
}

pub(super) fn record_applied_assessment(
    mut task: ValidatedTask,
    snapshot: &PreparedTaskSnapshot,
    outcome: &str,
    task_results: &mut Vec<Value>,
    ci_sweep_admission: &mut Vec<Value>,
    drain_approval: &mut Vec<Value>,
) {
    let changed =
        snapshot.context_files != task.after || snapshot.complexity != Some(task.complexity);
    let (field, decision, records) = match task.admission {
        Some(Admission::CiSweep(decision)) => (
            Some("ci_sweep_admission"),
            Some(decision),
            ci_sweep_admission,
        ),
        Some(Admission::Drain(decision)) => {
            (Some("drain_approval"), Some(decision), drain_approval)
        }
        None => (None, None, ci_sweep_admission),
    };
    let approved = decision
        .as_ref()
        .is_some_and(|decision| decision["approved"] == true);
    if let Value::Object(fields) = &mut task.assessment {
        fields.insert(
            "applied".to_string(),
            Value::Bool(changed || task.promote || approved),
        );
        fields.insert("outcome".to_string(), json!(outcome));
        fields.insert("operation_id".to_string(), json!(task.operation_id));
        if let (Some(field), Some(decision)) = (field, decision.clone()) {
            fields.insert(field.to_string(), decision);
        }
    }
    if let Some(decision) = decision {
        records.push(decision);
    }
    task_results.push(task.assessment);
}

pub(super) fn with_task_locks(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    index: usize,
    operation: &mut dyn FnMut() -> Result<(), OrbitError>,
) -> Result<(), OrbitError> {
    let Some(task_id) = task_ids.get(index) else {
        return operation();
    };
    let mut nested = || with_task_locks(runtime, task_ids, index + 1, operation);
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(task_id, &mut nested)
}

fn task_snapshot_drift(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
) -> Option<(&'static str, &'static str)> {
    if snapshot
        .material
        .as_ref()
        .is_some_and(|(expected, revision)| {
            crate::application::automation::preparation::fingerprint(
                runtime, current, revision, policy,
            )
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

/// Why `current` no longer carries the material a pilot assessed from
/// `snapshot` and then wrote as `after` and `complexity`, if it does not.
/// Status is the caller's to judge: the pilot's own fields are restored to
/// their prepared values and the rest is held to the same freshness contract
/// as the pilot write, including its tolerance for dependency status moves.
pub(super) fn assessed_material_drift(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    after: &[String],
    complexity: TaskComplexity,
    policy: &PreparationPolicy,
) -> Option<(&'static str, String)> {
    if current.context_files != after {
        return Some((
            "context_files_changed",
            "task context_files changed after the pilot wrote them".to_string(),
        ));
    }
    if current.complexity != Some(complexity) {
        return Some((
            "complexity_changed",
            "task complexity changed after the pilot wrote it".to_string(),
        ));
    }
    let mut assessed = current.clone();
    assessed.context_files = snapshot.context_files.clone();
    assessed.complexity = snapshot.complexity;
    assessed.status = snapshot.status;
    let (reason, detail) = task_snapshot_drift(runtime, &assessed, snapshot, policy)?;
    if reason != "material_changed" {
        return Some((reason, detail.to_string()));
    }
    let Some(fingerprint) = status_only_fingerprint(runtime, &assessed, snapshot, policy) else {
        return Some((
            reason,
            material_change_detail(runtime, &assessed, snapshot, policy),
        ));
    };
    let mut rebased = snapshot.clone();
    rebased.material = rebased
        .material
        .map(|(_, revision)| (fingerprint, revision));
    task_snapshot_drift(runtime, &assessed, &rebased, policy)
        .map(|(reason, detail)| (reason, detail.to_string()))
}

pub(super) fn stale_task(task_id: &str, reason: &str, detail: &str) -> Value {
    json!({
        "task_id": task_id,
        "outcome": "stale",
        "reason": reason,
        "detail": detail,
    })
}

/// A newer owner can make a prepared assessment unnecessary without making
/// the pilot fail. Durable task edits still refuse the stale write, but settle
/// it as superseded, as do admission, execution claims and terminal decisions.
pub(super) fn superseded_task(
    runtime: &OrbitRuntime,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
    ci_sweep: bool,
) -> Result<Option<Value>, OrbitError> {
    if !matches!(snapshot.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(None);
    }
    let current = match runtime.get_task(task_id) {
        Ok(task) => task,
        Err(OrbitError::NotFound { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let superseded = |reason: &str, detail: &str| {
        json!({
            "task_id": task_id, "outcome": "superseded", "reason": reason,
            "status": current.status, "detail": detail,
        })
    };
    if ci_sweep && matches!(current.status, TaskStatus::Rejected | TaskStatus::Archived) {
        return Ok(Some(superseded(
            "operator_rejected",
            "the operator rejected or archived the CI-sweep task before admission",
        )));
    }
    if ci_sweep && current.status != snapshot.status {
        // CI-sweep authority only treats operator rejection or archival as a
        // benign race. Other status changes continue through admission and
        // fail its existing proposed-status check.
        return Ok(None);
    }
    if !ci_sweep
        && matches!(
            current.status,
            TaskStatus::Done | TaskStatus::Rejected | TaskStatus::Archived
        )
    {
        return Ok(Some(superseded(
            "task_terminal",
            "task became terminal after preparation",
        )));
    }
    if matches!(current.status, TaskStatus::InProgress | TaskStatus::Review)
        && runtime
            .inspect_execution_claims()?
            .iter()
            .any(|inspection| {
                inspection.claim.task_id == task_id && inspection.claim.phase.protects_footprint()
            })
    {
        return Ok(Some(superseded(
            "execution_claim",
            "an execution claim superseded preparation",
        )));
    }
    let history = runtime.get_task_history(task_id)?;
    let newer = &history[snapshot.history_len.unwrap_or(0).min(history.len())..];
    let edited = snapshot.history_len.is_some()
        && newer.iter().any(|entry| {
            // The pilot's own atomic mutation can re-seal a creation grant.
            // Its replay receipt remains authoritative for that same write.
            entry.by != "task-pilot"
                && matches!(
                    entry.event.as_str(),
                    "updated"
                        | "renamed"
                        | "crew_assigned"
                        | "crew_redrawn"
                        | "context_creation_authorized"
                )
        });
    let structural_drift = current.context_files != snapshot.context_files
        || current.complexity != snapshot.complexity
        || current.title != snapshot.title
        || current.tags != snapshot.tags;
    let creation_drift =
        runtime.context_creation_state(&current)?.identity() != snapshot.context_creation_identity;
    if edited {
        let material_drift = if let Some((_, revision)) = &snapshot.material {
            let fresh = crate::application::automation::preparation::fingerprints(
                runtime, &current, revision, policy,
            )
            .map_err(orbit_automation::automation_error_to_orbit)?;
            snapshot
                .status_neutral_fingerprint
                .as_ref()
                .is_some_and(|expected| expected != &fresh.status_neutral)
        } else {
            // No Git material hash: an actual field update after the captured
            // history boundary is sufficient to withhold this old assessment.
            true
        };
        if structural_drift || material_drift {
            let (reason, detail) = task_snapshot_drift(runtime, &current, snapshot, policy)
                .unwrap_or(("task_edited", "task fields changed after preparation"));
            let detail = if reason == "material_changed" {
                material_change_detail(runtime, &current, snapshot, policy)
            } else {
                detail.to_string()
            };
            return Ok(Some(superseded(reason, &detail)));
        }
        if creation_drift {
            return Ok(Some(superseded(
                CONTEXT_CREATION_CHANGED,
                CONTEXT_CREATION_CHANGED_DETAIL,
            )));
        }
    }
    if snapshot.history_len.is_some()
        && !matches!(current.status, TaskStatus::Proposed | TaskStatus::Backlog)
        && newer
            .iter()
            .any(|entry| entry.to_status == Some(current.status))
    {
        return Ok(Some(superseded(
            "status_changed",
            "a durable task transition superseded preparation",
        )));
    }
    if matches!(current.status, TaskStatus::InProgress | TaskStatus::Review)
        && !structural_drift
        && !creation_drift
        && newer.iter().any(|entry| {
            entry.to_status == Some(TaskStatus::InProgress)
                && entry.by == "system"
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.starts_with("workflow admission:"))
        })
        && (status_only_fingerprint(runtime, &current, snapshot, policy).is_some()
            || (snapshot.material.is_none() && snapshot.history_len.is_some() && !edited))
    {
        return Ok(Some(superseded(
            "workflow_admission",
            "workflow admission superseded preparation",
        )));
    }
    Ok(None)
}

pub(super) fn task_outcome(task_id: &str, outcome: &str, error: Option<String>) -> Value {
    json!({ "task_id": task_id, "outcome": outcome, "error": error })
}

/// The branch changed this task's material after its claim froze the
/// source: the assessment is not applied, and the member is claimed afresh
/// at the head rather than retried or retired [ORB-14476].
pub(super) fn source_superseded(task_id: &str, detail: &str) -> Value {
    json!({
        "task_id": task_id,
        "outcome": "superseded",
        "reason": SUPERSEDED_BY_SOURCE,
        "detail": detail,
    })
}

/// A partition whose result is unusable fails its tasks as invalid, except
/// those `superseded` by a source move, which settle superseded instead.
pub(super) fn failed_partition(
    partition_index: u64,
    task_ids: &[String],
    error: String,
    superseded: &BTreeMap<String, String>,
) -> Value {
    let task_outcomes = task_ids
        .iter()
        .map(|task_id| match superseded.get(task_id) {
            Some(detail) => source_superseded(task_id, detail),
            None => task_outcome(task_id, "invalid", Some(error.clone())),
        })
        .collect::<Vec<_>>();
    let unresolved = task_ids
        .iter()
        .filter(|task_id| !superseded.contains_key(*task_id))
        .count();
    json!({
        "partition_index": partition_index,
        "task_ids": task_ids,
        "outcome": if unresolved == 0 { "superseded" } else { "failed" },
        "error": (unresolved > 0).then_some(error),
        "task_outcomes": task_outcomes,
        "unresolved_count": unresolved,
        "applied_task_ids": [],
    })
}
