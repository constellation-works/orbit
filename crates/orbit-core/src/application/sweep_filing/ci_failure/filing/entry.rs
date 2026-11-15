//! Entry points: the runtime-bound `file_ci_failure_tasks`, which records a
//! deferred outcome as a run incident, and `file_ci_failure_tasks_with_ops`,
//! which runs the filing phases in order.

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::sweep_filing::duplicate_tasks::DuplicateTaskLookup;
use crate::application::task::TaskAddParams;

use super::super::branch_observation::{note_branch_retention, retain_branch_observations};
use super::OUTCOME_DEFERRED;
use super::assess::{ClusterAssessments, assess_clusters};
use super::file_clusters::{dependency_fold, file_clusters};
use super::report::{current_failures_outcome, empty_clusters_outcome};
use super::snapshot::{SnapshotPreparation, prepare_snapshot, register_clusters};

pub(crate) fn file_ci_failure_tasks(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    let output = file_ci_failure_tasks_with_ops(runtime, input, runtime, |params| {
        runtime.add_task(params).map(|task| task.id)
    })?;
    // [ORB-15517] A sweep that deferred its findings succeeds, so Incidents
    // learns of the deferral from its own row rather than a failed run.
    if output["outcome"] == OUTCOME_DEFERRED {
        let run_id = ["job_run_id", "run_id"]
            .into_iter()
            .find_map(|key| input.get(key).and_then(Value::as_str));
        if output.get("stage").and_then(Value::as_str) == Some("collection_or_investigation") {
            runtime.record_run_deferral(
                run_id,
                OUTCOME_DEFERRED,
                "ci_failure_sweep deferred because collection was incomplete due to \
                 retryable errors",
                json!({
                    "stage": output["stage"],
                    "retryable_errors": output["retryable_errors"],
                    "audit": output["audit"],
                }),
            );
        } else if output["rate_limit"].is_object() {
            runtime.record_run_deferral(
                run_id,
                OUTCOME_DEFERRED,
                "ci_failure_sweep deferred to GitHub's rate-limit reset",
                json!({"rate_limit": output["rate_limit"]}),
            );
        } else {
            runtime.record_run_deferral(
                run_id,
                OUTCOME_DEFERRED,
                "ci_failure_sweep deferred current failures with incomplete evidence to a later \
                 sweep",
                json!({
                    "deferred_count": output["deferred_count"],
                    "deferred_failure_run_ids": output["audit"]["deferred_failure_run_ids"],
                }),
            );
        }
    }
    Ok(output)
}

pub(crate) fn file_ci_failure_tasks_with_ops<L, F>(
    runtime: &OrbitRuntime,
    input: &Value,
    lookup: &L,
    mut add_task: F,
) -> Result<Value, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
    F: FnMut(TaskAddParams) -> Result<String, OrbitError>,
{
    let mut snapshot = match prepare_snapshot(runtime, input, lookup, &mut add_task)? {
        SnapshotPreparation::Finished(output) => return Ok(output),
        SnapshotPreparation::Ready(snapshot) => *snapshot,
    };
    let clusters = register_clusters(&snapshot)?;

    // Retain before either return. One claimed owner or one bad receipt must
    // not drop landing repairs, and a later lookup failure must not lose the
    // observations already classified.
    let retention = retain_branch_observations(runtime, &snapshot.branch_observations);
    note_branch_retention(&mut snapshot.audit, &retention);

    if clusters.is_empty() {
        return empty_clusters_outcome(snapshot, retention);
    }

    let ClusterAssessments {
        owners,
        repair_assessments,
    } = assess_clusters(runtime, lookup, &clusters, &snapshot.audit)?;
    let fold = dependency_fold(&clusters, &owners, snapshot.max_tasks);
    let mut deferred_attribution = retention.deferred_attribution;
    let filed = file_clusters(
        runtime,
        &clusters,
        owners,
        &fold,
        &mut snapshot,
        &mut deferred_attribution,
        &mut add_task,
    )?;
    current_failures_outcome(
        snapshot,
        &clusters,
        &fold,
        filed,
        retention.attributed,
        deferred_attribution,
        repair_assessments,
    )
}
