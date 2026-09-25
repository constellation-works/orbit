//! Evidence completeness: which findings may be filed now and which stay
//! deferred, plus the audit and retryable-error shapes filing reports.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use serde_json::{Value, json};

use super::fields::{run_order, value_string};
use super::log_signature::error_signature;

/// The run a snapshot entry — a retryable error or a current failure — is
/// about, as a comparable key. Collection emits a numeric `run_id`; the
/// version-1 `query_errors` shape used a string.
pub(super) fn run_id_key(entry: &Value) -> Option<String> {
    match entry.get("run_id") {
        Some(Value::Number(number)) => Some(number.to_string()),
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

/// Split retryable errors by blast radius.
///
/// A job error affects only that job; a run error affects all its jobs. An
/// error that names none — a repository read, a run listing, a pull-request
/// listing — leaves the whole snapshot in doubt: any finding it did produce
/// could be missing the newer run that would have superseded it, so filing
/// from that snapshot is not safe.
pub(super) fn partition_retryable_errors(
    errors: &[Value],
) -> (usize, BTreeMap<String, Vec<Value>>) {
    let mut snapshot_wide = 0usize;
    let mut by_run: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for error in errors {
        match run_id_key(error) {
            Some(run_id) => by_run.entry(run_id).or_default().push(error.clone()),
            None => snapshot_wide += 1,
        }
    }
    (snapshot_wide, by_run)
}

/// Separate the failures that can be filed from the ones whose evidence is
/// incomplete.
///
/// Per-finding evidence requirements are unchanged: a failure is filed only
/// when collection investigated it fully and no applicable job or run error
/// is recorded. What changes is that a deferred failure now says so in its own entry
/// instead of silently withholding its neighbours.
pub(super) fn split_deferred_failures(
    failures: &[Value],
    run_errors: &BTreeMap<String, Vec<Value>>,
    schema_version: u64,
) -> (Vec<Value>, Vec<Value>) {
    let mut complete = Vec::new();
    let mut deferred = Vec::new();
    for failure in failures {
        let mut reasons = run_id_key(failure)
            .and_then(|run_id| run_errors.get(&run_id))
            .into_iter()
            .flatten()
            .filter(|error| {
                error.get("job_id").is_none_or(Value::is_null)
                    || error.get("job_id") == failure.get("job_id")
            })
            .cloned()
            .collect::<Vec<_>>();
        if reasons.is_empty()
            && failure.get("investigated").and_then(Value::as_bool) == Some(true)
            && let Some(message) = job_evidence_gap(failure, schema_version)
        {
            reasons.push(json!({
                "stage": "registration", "operation": "job_evidence_identity",
                "run_id": failure.get("run_id"), "job_id": failure.get("job_id"),
                "retryable": true, "message": message,
            }));
        }
        let investigated = failure.get("investigated").and_then(Value::as_bool) == Some(true);
        if reasons.is_empty() && investigated {
            complete.push(failure.clone());
            continue;
        }
        deferred.push(json!({
            "job_id": failure.get("job_id"),
            "failed_jobs": failure.get("failed_jobs"),
            "run_id": failure.get("run_id"),
            "url": failure.get("url"),
            "workflow": failure.get("workflow"),
            "head_branch": failure.get("head_branch"),
            "ref_kind": failure.get("ref_kind"),
            "investigated": investigated,
            "retryable": true,
            "reasons": if reasons.is_empty() {
                vec![json!({
                    "stage": "registration",
                    "operation": "current_failure_not_investigated",
                    "run_id": failure.get("run_id"),
                    "retryable": true,
                    "message": "a current CI failure has no complete investigation and cannot be filed safely",
                })]
            } else {
                reasons
            },
        }));
    }
    (complete, deferred)
}

/// A newer green push on the same branch is stronger evidence than an older
/// red finding. The collector normally moves that red run to
/// `stale_or_superseded`; retaining this check at filing keeps a replayed or
/// hand-constructed snapshot from filing a repair after the branch is already
/// green.
pub(super) fn exclude_already_repaired(
    failures: Vec<Value>,
    evidence: &Value,
) -> (Vec<Value>, Vec<Value>) {
    let mut remaining = Vec::new();
    let mut repaired = Vec::new();
    for failure in failures {
        let Some(green) = newer_green_push_run(&failure, evidence) else {
            remaining.push(failure);
            continue;
        };
        repaired.push(json!({
            "run_id": failure.get("run_id"),
            "workflow": failure.get("workflow"),
            "head_branch": failure.get("head_branch"),
            "reason": "newer_push_run_green",
            "superseded_by": green,
        }));
    }
    let mut repaired_ids = repaired
        .iter()
        .filter_map(run_id_key)
        .collect::<BTreeSet<_>>();
    for stale in evidence
        .get("stale_or_superseded")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if repaired_ids.contains(&run_id_key(stale).unwrap_or_default()) {
            continue;
        }
        let Some(green) = newer_green_push_run(stale, evidence) else {
            continue;
        };
        repaired.push(json!({
            "run_id": stale.get("run_id"),
            "workflow": stale.get("workflow"),
            "head_branch": stale.get("head_branch"),
            "reason": "newer_push_run_green",
            "superseded_by": green,
        }));
        if let Some(run_id) = run_id_key(stale) {
            repaired_ids.insert(run_id);
        }
    }
    (remaining, repaired)
}

fn newer_green_push_run<'a>(failure: &Value, evidence: &'a Value) -> Option<&'a Value> {
    let workflow = value_string(failure, "workflow");
    let branch = value_string(failure, "head_branch");
    let failure_order = run_order(failure);
    let runs = evidence
        .get("latest_runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    runs.filter(|run| {
        value_string(run, "workflow") == workflow
            && value_string(run, "head_branch") == branch
            && value_string(run, "event") == "push"
            && run_order(run) > failure_order
            && run_is_completed_success(run)
    })
    .max_by_key(|run| run_order(run))
    .or_else(|| superseding_green_run(failure, evidence))
}

fn superseding_green_run<'a>(failure: &Value, evidence: &'a Value) -> Option<&'a Value> {
    let stale = evidence
        .get("stale_or_superseded")
        .and_then(Value::as_array)?;
    stale.iter().find_map(|entry| {
        if run_id_key(entry) != run_id_key(failure)
            || value_string(entry, "workflow") != value_string(failure, "workflow")
            || value_string(entry, "head_branch") != value_string(failure, "head_branch")
        {
            return None;
        }
        let superseded_by = entry.get("superseded_by")?;
        (value_string(superseded_by, "event") == "push" && run_is_completed_success(superseded_by))
            .then_some(superseded_by)
    })
}

fn run_is_completed_success(run: &Value) -> bool {
    run.get("status").and_then(Value::as_str) == Some("completed")
        && matches!(
            run.get("conclusion").and_then(Value::as_str),
            Some("success" | "neutral" | "skipped")
        )
}

pub(super) fn repaired_audit(mut audit: Value, already_repaired: &[Value]) -> Value {
    audit["already_repaired_count"] = json!(already_repaired.len());
    audit["already_repaired_run_ids"] = json!(
        already_repaired
            .iter()
            .filter_map(|entry| entry.get("run_id").cloned())
            .collect::<Vec<_>>()
    );
    audit
}

/// Old snapshots did not bind the run log or its checkout scan to the named
/// job. They remain readable audit evidence, but must be recollected before
/// filing; inferring attribution from job order would repeat the original bug.
fn job_evidence_gap(failure: &Value, schema_version: u64) -> Option<&'static str> {
    if schema_version < 2 {
        return Some("legacy run-scoped evidence has no verified job binding; recollect this run");
    }
    let Some(job_id) = failure.get("job_id").and_then(Value::as_u64) else {
        return Some("failure has no numeric job identity");
    };
    let jobs = failure.get("failed_jobs").and_then(Value::as_array);
    let Some(job) = jobs
        .filter(|jobs| jobs.len() == 1)
        .and_then(|jobs| jobs.first())
    else {
        return Some("failure must identify exactly one supplying job");
    };
    if job.get("job_id").and_then(Value::as_u64) != Some(job_id)
        || failure.get("log_job_id").and_then(Value::as_u64) != Some(job_id)
    {
        return Some("diagnostic evidence is not bound to the named job");
    }
    if job
        .get("failed_steps")
        .and_then(Value::as_array)
        .is_none_or(|steps| steps.len() != 1)
    {
        return Some("failed step identity is missing or ambiguous within this job");
    }
    if failure["diagnostic_unit"]["kind"] == "runner_failure_regions"
        && selected_diagnostic(failure).is_none()
    {
        return Some(
            "failure regions have invalid completeness, omission accounting or attribution",
        );
    }
    if let Some(text) = selected_diagnostic(failure)
        && error_signature(text, &value_string(&job["failed_steps"][0], "name")).step_fallback
    {
        return Some("selected evidence contains no concrete diagnostic");
    }
    if failure["log_source_complete"] == false {
        return Some("job log source is incomplete");
    }
    if selected_diagnostic(failure).is_none()
        && (value_string(failure, "log_excerpt").trim().is_empty()
            || failure.get("log_truncated").and_then(Value::as_bool) != Some(false))
    {
        return Some("job diagnostic evidence is missing or truncated");
    }
    if value_string(failure, "log_source") == "job_api_log"
        && failure
            .get("log_source_jobs")
            .and_then(Value::as_array)
            .is_none_or(|jobs| jobs.len() != 1 || jobs[0]["job_id"].as_u64() != Some(job_id))
    {
        return Some("fallback log evidence belongs to a different or unknown job");
    }
    let identity = &failure["checkout_identity"];
    if identity["provenance"]["job_id"].as_u64() != Some(job_id)
        || identity["provenance"]["complete"].as_bool() != Some(true)
        || identity["state"] != "observed"
        || failure
            .get("actual_checkout_shas")
            .and_then(Value::as_array)
            .is_none_or(|shas| shas.len() != 1)
    {
        return Some("checkout identity is not completely observed for this job");
    }
    None
}

/// Additive schema-2 evidence. Old snapshots remain conservative when their
/// display was truncated; a complete command or explicitly partial failure
/// regions from a completely scanned command can replace that display.
pub(super) fn selected_diagnostic(failure: &Value) -> Option<&str> {
    let unit = &failure["diagnostic_unit"];
    let job = failure["failed_jobs"].as_array()?.first()?;
    let step = job["failed_steps"].as_array()?.first()?["name"].as_str()?;
    let text = unit["text"].as_str()?;
    let complete_command = unit["kind"] == "runner_command" && unit["complete"] == true;
    let failure_regions = valid_failure_regions(unit) && failure["log_source_complete"] == true;
    ((complete_command || failure_regions)
        && unit["job_id"].as_u64()? == failure["job_id"].as_u64()?
        && unit["step"].as_str()? == step
        && !text.trim().is_empty()
        && text.len() <= 262_144)
        .then_some(text)
}

/// Region completeness describes selection and command boundaries, never full
/// retention. Reject malformed or contradictory omission accounting at filing.
pub(super) fn valid_failure_regions(unit: &Value) -> bool {
    let Some(total) = unit["command_bytes"].as_u64() else {
        return false;
    };
    let Some(retained) = unit["retained_source_bytes"].as_u64() else {
        return false;
    };
    let Some(omitted) = unit["omitted_bytes"].as_u64() else {
        return false;
    };
    let Some(assertions) = unit["assertion_payload_omitted_bytes"].as_u64() else {
        return false;
    };
    unit["kind"] == "runner_failure_regions"
        && unit["complete"] == false
        && unit["command_complete"] == true
        && unit["selection_complete"] == true
        && retained > 0
        && omitted > 0
        && retained.checked_add(omitted) == Some(total)
        && assertions <= omitted
        && unit["failure_anchor_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
        && unit["text"].as_str().is_some_and(|text| {
            text.len() <= 65_536 && unit["returned_bytes"].as_u64() == Some(text.len() as u64)
        })
}

/// The deferred entries flattened back into the error list shape, for the
/// ending where nothing could be filed at all.
pub(super) fn deferred_errors(deferred: &[Value]) -> Vec<Value> {
    deferred
        .iter()
        .filter_map(|entry| entry.get("reasons").and_then(Value::as_array))
        .flat_map(|reasons| reasons.iter().cloned())
        .collect()
}

/// Make partial registration legible: an operator reading the audit must be
/// able to tell "three findings, three filed" from "three findings filed and
/// eleven still owed".
pub(super) fn deferral_audit(mut audit: Value, deferred: &[Value]) -> Value {
    audit["deferred_failures"] = json!(deferred.len());
    audit["deferred_failure_run_ids"] = json!(
        deferred
            .iter()
            .filter_map(|entry| entry.get("run_id").cloned())
            .collect::<Vec<_>>()
    );
    audit["retryable_errors"] = json!(deferred_errors(deferred).len());
    audit
}

pub(super) fn audit_summary(evidence: &Value, failures: &[Value]) -> Value {
    let latest_run_ids = evidence
        .get("latest_runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|run| run.get("run_id").cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let current_failure_run_ids = failures
        .iter()
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    let investigated_failure_run_ids = failures
        .iter()
        .filter(|run| run.get("investigated").and_then(Value::as_bool) == Some(true))
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    json!({
        "latest_runs_discovered": latest_run_ids.len(),
        "latest_run_ids": latest_run_ids,
        "current_failures": current_failure_run_ids.len(),
        "current_failure_run_ids": current_failure_run_ids,
        "investigated_failures": investigated_failure_run_ids.len(),
        "investigated_failure_run_ids": investigated_failure_run_ids,
        "tasks_created": 0,
        "created_task_ids": [],
        "existing_task_skips": 0,
        "existing_task_owners": [],
        "deferred_failures": 0,
        "deferred_failure_run_ids": [],
        "retryable_errors": 0,
        "already_repaired_count": 0,
        "already_repaired_run_ids": [],
    })
}

pub(super) fn filing_audit(mut audit: Value, filed: &[Value], skipped_existing: &[Value]) -> Value {
    let created_task_ids = filed
        .iter()
        .filter_map(|entry| entry.get("task_id").cloned())
        .collect::<Vec<_>>();
    let existing_task_owners = skipped_existing
        .iter()
        .filter_map(|entry| entry.get("task_id").cloned())
        .collect::<Vec<_>>();
    audit["tasks_created"] = json!(created_task_ids.len());
    audit["created_task_ids"] = json!(created_task_ids);
    audit["existing_task_skips"] = json!(skipped_existing.len());
    audit["existing_task_owners"] = json!(existing_task_owners);
    audit
}

pub(super) fn retryable_pipeline_error(
    stage: &str,
    audit: &Value,
    errors: Vec<Value>,
) -> OrbitError {
    let mut audit = audit.clone();
    audit["retryable_errors"] = json!(errors.len());
    OrbitError::Execution(format!(
        "ci_failure_sweep retryable: {}",
        json!({
            "outcome": "retryable_error",
            "stage": stage,
            "audit": audit,
            "errors": errors,
        })
    ))
}

pub(super) fn bounded_error(message: &str) -> String {
    redact_all(message).chars().take(500).collect()
}

pub(super) fn normalize_retryable_error(error: Value) -> Value {
    json!({
        "stage": error.get("stage").and_then(Value::as_str).unwrap_or("collection"),
        "operation": error
            .get("operation")
            .or_else(|| error.get("query"))
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        "run_id": error.get("run_id").cloned().unwrap_or(Value::Null),
        "job_id": error.get("job_id").cloned().unwrap_or(Value::Null),
        "retryable": true,
        "message": bounded_error(
            error
                .get("message")
                .or_else(|| error.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("CI handoff operation failed"),
        ),
    })
}
