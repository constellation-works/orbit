//! Cancelled CI jobs that never reached a failed step: inconclusive, not filed.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::evidence::run_id_key;

fn job_is_cancelled_without_failed_steps(job: &Value) -> bool {
    job.get("conclusion").and_then(Value::as_str) == Some("cancelled")
        && job
            .get("failed_steps")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
}

fn is_inconclusive_cancellation(failure: &Value) -> bool {
    if failure.get("evidence_state").and_then(Value::as_str) == Some("inconclusive") {
        return true;
    }
    match failure.get("failed_jobs").and_then(Value::as_array) {
        Some(jobs) if !jobs.is_empty() => jobs.iter().all(job_is_cancelled_without_failed_steps),
        Some(_) | None => {
            failure.get("conclusion").and_then(Value::as_str) == Some("cancelled")
                && failure.get("investigated").and_then(Value::as_bool) == Some(true)
        }
    }
}

pub(super) fn split_inconclusive_cancellations(
    failures: Vec<Value>,
    mut inconclusive: Vec<Value>,
) -> (Vec<Value>, Vec<Value>) {
    let mut seen: BTreeSet<(Option<String>, Option<u64>)> = inconclusive
        .iter()
        .map(|entry| {
            (
                run_id_key(entry),
                entry.get("job_id").and_then(Value::as_u64),
            )
        })
        .collect();
    let mut remaining = Vec::new();
    for failure in failures {
        if is_inconclusive_cancellation(&failure) {
            let identity = (
                run_id_key(&failure),
                failure.get("job_id").and_then(Value::as_u64),
            );
            if seen.insert(identity) {
                inconclusive.push(failure);
            }
        } else {
            remaining.push(failure);
        }
    }
    (remaining, inconclusive)
}

fn log_or_checkout_investigation_operation(operation: &str) -> bool {
    matches!(
        operation,
        "run_logs"
            | "run_logs_all"
            | "checkout_evidence"
            | "checkout_evidence_budget"
            | "job_log_truncated"
            | "job_log_budget"
    )
}

/// Absent logs for a cancelled job with no failed steps are not a repair
/// gap. Keep transport, auth, listing, and genuine-failure log errors.
pub(super) fn drop_inconclusive_log_errors(
    errors: Vec<Value>,
    inconclusive: &[Value],
) -> Vec<Value> {
    let inconclusive_runs: BTreeSet<String> = inconclusive.iter().filter_map(run_id_key).collect();
    let inconclusive_jobs: BTreeSet<(String, u64)> = inconclusive
        .iter()
        .filter_map(|entry| {
            Some((
                run_id_key(entry)?,
                entry.get("job_id").and_then(Value::as_u64)?,
            ))
        })
        .collect();
    errors
        .into_iter()
        .filter(|error| {
            let operation = error
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !log_or_checkout_investigation_operation(operation) {
                return true;
            }
            let Some(run_id) = run_id_key(error) else {
                return true;
            };
            match error.get("job_id").and_then(Value::as_u64) {
                Some(job_id) => !inconclusive_jobs.contains(&(run_id, job_id)),
                None => !inconclusive_runs.contains(&run_id),
            }
        })
        .collect()
}

pub(super) fn inconclusive_audit(mut audit: Value, inconclusive: &[Value]) -> Value {
    audit["inconclusive"] = json!(inconclusive.len());
    audit["inconclusive_run_ids"] = json!(
        inconclusive
            .iter()
            .filter_map(|entry| entry.get("run_id").cloned())
            .collect::<Vec<_>>()
    );
    audit["inconclusive_job_ids"] = json!(
        inconclusive
            .iter()
            .filter_map(|entry| entry.get("job_id").cloned())
            .filter(|value| !value.is_null())
            .collect::<Vec<_>>()
    );
    audit
}
