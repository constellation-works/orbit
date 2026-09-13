//! `file_ci_failure_tasks` — turn one CI evidence snapshot into quarantined
//! proposed tasks.
//!
//! The snapshot arrives from the host-owned `collect_ci_evidence` step, which
//! ran `gh` outside any agent sandbox. Filing combines that JSON with workspace
//! task/run evidence and Git applicability: cluster current failures by
//! root cause, reuse open owners or completed owners with verified repair
//! evidence, and file what is left as `proposed` bug tasks with inline evidence.
//! The CI sweep then pilots and revalidates those tasks before a
//! separate admission boundary may expose them to backlog auto-drain.
//!
//! A filed task deliberately has no `required_tools`: the pilot and eventual
//! implementation never have to query GitHub, because the original evidence
//! is already in the description. It is not executable until a successful
//! pilot has applied valid selectors, found the failure relevant to current
//! integration code, and exercised explicit sweep promotion authority.
//!
//! # Two keys, on purpose
//!
//! `cluster_key` includes the commit the runner actually tested, so one
//! regression observed across a push run and a pull-request run of the *same*
//! commit collapses into one task instead of two.
//!
//! Ordinary `failure_key` tags omit the commit to keep a still-open repair
//! across branch advances. Proven compiler causes instead include the exact
//! diagnostic set, source locations and observed checkout, omitting job and
//! workflow wrappers. That conservative proof consolidates cross-job failures
//! without conflating different compiler operands or source revisions. Shipped
//! per-job tags remain readable for the same immutable supplying evidence.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_tools::github_cli::strip_ansi_sequences;
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::duplicate_tasks::{
    CoverageAnchor, CoverageFingerprint, DuplicateCandidate, DuplicateTaskLookup,
    DuplicateTaskMatch, find_covering_task,
};
use crate::application::task::TaskAddParams;

#[path = "ci_repair_assessment.rs"]
mod repair_assessment;

/// Wire contract with `collect_ci_evidence` (`orbit-engine`'s
/// `executor::automation::ci`), also stated in both activity assets' schemas.
/// The three endings must never collapse into one another, and none of them is
/// a CI pass.
const OUTCOME_CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";
const OUTCOME_NO_CURRENT_FAILURE: &str = "no_current_failure";
const OUTCOME_CURRENT_FAILURES: &str = "current_failures";

/// Snapshot schema this step knows how to read.
const SUPPORTED_SCHEMA_VERSION: u64 = 2;

/// Provenance tag: every task this step files carries it.
pub(crate) const CI_FAILURE_TAG: &str = "ci-failure-sweep";
/// Prefix of the dedupe tag, completed by the failure key.
pub(crate) const CI_FAILURE_KEY_TAG_PREFIX: &str = "ci-failure:";
/// Title prefix on every task this step files, so a sweep-filed task is
/// identifiable in a backlog listing without reading its tags.
const CI_FAILURE_SWEEP_TITLE_PREFIX: &str = "[ci-failure-sweep] ";
/// The system crew name. Filed tasks belong on the system lane, matching the
/// shipped `ci-failure-remediation` auto-task — but this is a plain default,
/// not a hard-coded assumption that the lane is configured: a workspace whose
/// crew roster has no `system` entry still gets its task filed, just without
/// a crew set.
const SYSTEM_CREW: &str = "system";

const DEFAULT_MAX_TASKS: u64 = 5;
const MAX_MAX_TASKS: u64 = 20;
/// Hex characters of the signature digest kept in a tag. Full-width digests
/// make a tag unreadable in a task list; this is a dedupe key, not a security
/// boundary.
const KEY_LEN: usize = 16;
/// Log bytes carried into a task description. `collect_ci_evidence` has already
/// bounded and redacted the excerpt; this is a second, tighter bound so a
/// description stays a readable brief.
const DESCRIPTION_LOG_BYTES: usize = 4_000;
/// Runs listed per cluster in the description.
const MAX_LISTED_RUNS: usize = 6;

pub(crate) fn file_ci_failure_tasks(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    file_ci_failure_tasks_with_ops(runtime, input, runtime, |params| {
        runtime.add_task(params).map(|task| task.id)
    })
}

#[cfg(test)]
pub(in crate::adapter::engine_host::v2_host) fn file_ci_failure_tasks_with_add<F>(
    runtime: &OrbitRuntime,
    input: &Value,
    add_task: F,
) -> Result<Value, OrbitError>
where
    F: FnMut(TaskAddParams) -> Result<String, OrbitError>,
{
    file_ci_failure_tasks_with_ops(runtime, input, runtime, add_task)
}

#[cfg(test)]
pub(in crate::adapter::engine_host::v2_host) fn file_ci_failure_tasks_with_lookup<L>(
    runtime: &OrbitRuntime,
    input: &Value,
    lookup: &L,
) -> Result<Value, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    file_ci_failure_tasks_with_ops(runtime, input, lookup, |params| {
        runtime.add_task(params).map(|task| task.id)
    })
}

fn file_ci_failure_tasks_with_ops<L, F>(
    runtime: &OrbitRuntime,
    input: &Value,
    lookup: &L,
    mut add_task: F,
) -> Result<Value, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
    F: FnMut(TaskAddParams) -> Result<String, OrbitError>,
{
    let evidence = input.get("ci_evidence").ok_or_else(|| {
        OrbitError::InvalidInput(
            "file_ci_failure_tasks requires the `ci_evidence` snapshot produced by \
             collect_ci_evidence"
                .to_string(),
        )
    })?;
    if !evidence.is_object() {
        return Err(OrbitError::InvalidInput(
            "file_ci_failure_tasks requires `ci_evidence` to be the snapshot object".to_string(),
        ));
    }
    let schema_version = evidence
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if schema_version > SUPPORTED_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "ci_evidence schema version {schema_version} is newer than the supported version \
             {SUPPORTED_SCHEMA_VERSION}; refusing to file tasks from a snapshot this step \
             cannot read"
        )));
    }
    let capability = evidence.get("capability").cloned().unwrap_or(Value::Null);

    // The collection stage could not look. Every list below would be empty and
    // would read exactly like "nothing is failing" — a conclusion that needs
    // queries this host never ran.
    if evidence.get("collected").and_then(Value::as_bool) != Some(true) {
        let audit = audit_summary(evidence, &[]);
        return Ok(json!({
            "outcome": OUTCOME_CAPABILITY_UNAVAILABLE,
            "capability": capability,
            "clusters": 0,
            "filed_count": 0,
            "filed": [],
            "pilot_candidate_count": 0,
            "pilot_candidates": [],
            "skipped_existing": [],
            "skipped_over_cap": [],
            "deferred": [],
            "audit": audit,
            "detail": "no CI evidence was gathered, so no task was filed; this is not a CI pass",
        }));
    }

    let max_tasks = bounded_u64(input, "max_tasks", DEFAULT_MAX_TASKS, MAX_MAX_TASKS)? as usize;
    let (failures, inconclusive) = split_inconclusive_cancellations(
        evidence
            .get("current_failures")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        evidence
            .get("inconclusive")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    );
    let audit = audit_summary(evidence, &failures);
    let mut retryable_errors = evidence
        .get("retryable_errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Version-1 snapshots emitted `query_errors`. Treat them with the repaired
    // contract when an older collect step is still paired with this filer.
    retryable_errors.extend(
        evidence
            .get("query_errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    );
    retryable_errors = retryable_errors
        .into_iter()
        .map(normalize_retryable_error)
        .collect();
    retryable_errors = drop_inconclusive_log_errors(retryable_errors, &inconclusive);
    // A gap in one run's evidence is a fact about that run. Letting it also
    // withhold every complete finding in the same snapshot is how a sweep that
    // had three fully evidenced regressions in hand filed nothing at all.
    let (mut snapshot_wide, mut run_errors) = partition_retryable_errors(&retryable_errors);
    // An uninvestigated failure collection said nothing else about still needs
    // its own reason; one that already has a recorded cause keeps that cause
    // rather than being restated generically.
    for failure in &failures {
        if failure.get("investigated").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let run_id = run_id_key(failure);
        if run_id
            .as_ref()
            .is_some_and(|run_id| run_errors.contains_key(run_id))
        {
            continue;
        }
        let error = json!({
            "stage": "registration",
            "operation": "current_failure_not_investigated",
            "run_id": failure.get("run_id"),
            "job_id": failure.get("job_id"),
            "retryable": true,
            "message": "a current CI failure has no complete investigation and cannot be filed safely",
        });
        retryable_errors.push(error.clone());
        match run_id {
            Some(run_id) => run_errors.entry(run_id).or_default().push(error),
            // Without a run ID there is nothing to defer *to*: the finding
            // cannot be told apart from any other, so the snapshot is unsafe
            // to file from at all.
            None => snapshot_wide += 1,
        }
    }
    if snapshot_wide > 0 {
        // A snapshot this incomplete cannot be filed from at all: the listing
        // that failed may be exactly the one holding the newer run that would
        // have superseded a finding. Report every error, not just the
        // snapshot-wide ones, so one payload explains the whole sweep.
        return Err(retryable_pipeline_error(
            "collection_or_investigation",
            &audit,
            retryable_errors,
        ));
    }
    let (complete, mut deferred) = split_deferred_failures(&failures, &run_errors, schema_version);
    let (complete, already_repaired) = exclude_already_repaired(complete, evidence);
    let audit = repaired_audit(audit, &already_repaired);
    // A run-scoped retryable error whose run never made it into
    // `current_failures` at all — an in-flight run with an observed failed
    // job but logs collection could not read yet — has no failure row for
    // `split_deferred_failures` to attach it to. Losing it here is exactly
    // how that mixed state would read as a clean `no_current_failure` instead
    // of the retryable gap it is: surface it as its own deferred entry so the
    // run ID and reason stay visible for a later sweep.
    let matched_run_ids: BTreeSet<String> = failures.iter().filter_map(run_id_key).collect();
    let evidence_deferred = evidence
        .get("deferred")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (run_id, reasons) in &run_errors {
        if matched_run_ids.contains(run_id) {
            continue;
        }
        let from_evidence = evidence_deferred
            .iter()
            .find(|entry| run_id_key(entry).as_deref() == Some(run_id));
        let run_id_value = reasons
            .first()
            .and_then(|reason| reason.get("run_id"))
            .cloned()
            .or_else(|| from_evidence.and_then(|entry| entry.get("run_id")).cloned())
            .unwrap_or(Value::Null);
        deferred.push(json!({
            "run_id": run_id_value,
            "url": from_evidence.and_then(|entry| entry.get("url")).cloned().unwrap_or(Value::Null),
            "workflow": from_evidence.and_then(|entry| entry.get("workflow")).cloned().unwrap_or(Value::Null),
            "head_branch": from_evidence.and_then(|entry| entry.get("head_branch")).cloned().unwrap_or(Value::Null),
            "ref_kind": from_evidence.and_then(|entry| entry.get("ref_kind")).cloned().unwrap_or(Value::Null),
            "investigated": false,
            "retryable": true,
            "reasons": reasons,
        }));
    }
    let audit = inconclusive_audit(deferral_audit(audit, &deferred), &inconclusive);
    let clusters = cluster_failures(&complete);

    if !complete.is_empty() && clusters.is_empty() {
        return Err(retryable_pipeline_error(
            "registration",
            &audit,
            vec![json!({
                "stage": "registration",
                "operation": "cluster_failures",
                "retryable": true,
                "message": "current failures were discovered but none could be registered for task filing",
            })],
        ));
    }

    if clusters.is_empty() {
        // Nothing was complete enough to file. The gaps are the whole result,
        // so this stays a retryable error rather than a clean sweep.
        if !deferred.is_empty() {
            return Err(retryable_pipeline_error(
                "collection_or_investigation",
                &audit,
                deferred_errors(&deferred),
            ));
        }
        return Ok(json!({
            "outcome": OUTCOME_NO_CURRENT_FAILURE,
            "capability": capability,
            "clusters": 0,
            "filed_count": 0,
            "filed": [],
            "pilot_candidate_count": 0,
            "pilot_candidates": [],
            "skipped_existing": [],
            "skipped_over_cap": [],
            "deferred": [],
            "inconclusive": inconclusive,
            "already_repaired": already_repaired,
            "audit": audit,
            "detail": if inconclusive.is_empty() {
                "the queries ran and found no current, non-superseded failure"
            } else {
                "the queries ran and found no current, non-superseded failure; cancelled jobs without failed steps remain explicit inconclusive evidence, not a pass"
            },
        }));
    }

    let mut filed = Vec::new();
    let mut pilot_candidates = Vec::new();
    let mut pilot_candidate_ids = BTreeSet::new();
    let mut skipped_existing = Vec::new();
    let mut skipped_over_cap = Vec::new();
    // Two clusters in one snapshot can share a failure key when the same root
    // cause was tested at two commits. The first filing closes the second.
    let mut filed_keys: BTreeSet<String> = BTreeSet::new();
    // Probed once per sweep rather than assumed: a workspace whose crew
    // roster has no `system` entry still needs filing to succeed, degrading
    // the same way any other task with an unrecognized crew does instead of
    // failing the sweep.
    let system_crew = runtime
        .validate_crew_name(Some(SYSTEM_CREW))
        .is_ok()
        .then(|| SYSTEM_CREW.to_string());

    // Complete every external lookup before the first task write. A transient
    // duplicate-check failure must leave no partial filing or dedupe state.
    let mut assessor = repair_assessment::Assessor::new(runtime);
    let mut repair_assessments = Vec::new();
    let duplicate_matches = clusters
        .iter()
        .map(|cluster| {
            let existing = cluster.find_covering_task(lookup).map_err(|error| {
                retryable_pipeline_error(
                    "dedupe_lookup",
                    &audit,
                    vec![json!({
                        "stage": "registration",
                        "operation": "find_covering_task",
                        "failure_key": cluster.failure_key,
                        "retryable": true,
                        "message": bounded_error(&error.to_string()),
                    })],
                )
            })?;
            if existing.is_some() {
                return Ok(existing);
            }
            let assessment = assessor.assess(cluster);
            if !assessment.evidence.is_null() {
                repair_assessments.push(assessment.evidence.clone());
            }
            Ok(assessment.owner.map(|task_id| DuplicateTaskMatch {
                task_id,
                match_kind: "covered_by_repair",
                evidence: assessment.evidence,
            }))
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;
    let duplicate_tasks = duplicate_matches
        .iter()
        .map(|duplicate_match| {
            duplicate_match
                .as_ref()
                .map(|matched| {
                    runtime.get_task(&matched.task_id).map_err(|error| {
                        retryable_pipeline_error(
                            "pilot_candidate_lookup",
                            &audit,
                            vec![json!({
                                "stage": "registration",
                                "operation": "reload_duplicate_task",
                                "task_id": matched.task_id,
                                "retryable": true,
                                "message": bounded_error(&error.to_string()),
                            })],
                        )
                    })
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;

    for ((cluster, duplicate_match), duplicate_task) in
        clusters.iter().zip(duplicate_matches).zip(duplicate_tasks)
    {
        if let Some(DuplicateTaskMatch {
            task_id,
            match_kind,
            evidence,
        }) = duplicate_match
        {
            if match_kind == "covered_by_repair" {
                repair_assessment::retain(runtime, &task_id, &evidence)?;
            }
            if let Some(existing) = duplicate_task {
                let expected_key_tag =
                    format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key);
                if existing.status == TaskStatus::Proposed
                    && existing.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                    && existing.tags.iter().any(|tag| tag == &expected_key_tag)
                    && pilot_candidate_ids.insert(task_id.clone())
                {
                    pilot_candidates.push(cluster.filing_entry(&task_id));
                }
            }
            skipped_existing.push(json!({
                "failure_key": cluster.failure_key,
                "cluster_key": cluster.cluster_key,
                "task_id": task_id,
                "workflow": cluster.workflow,
                "match_kind": match_kind,
                "match_evidence": evidence,
                "sources": cluster.filing_entry(&task_id)["sources"],
            }));
            continue;
        }
        if filed_keys.contains(&cluster.failure_key) {
            skipped_existing.push(json!({
                "failure_key": cluster.failure_key,
                "cluster_key": cluster.cluster_key,
                "task_id": filed
                    .iter()
                    .find(|entry: &&Value| entry["failure_key"] == json!(cluster.failure_key))
                    .and_then(|entry| entry["task_id"].as_str())
                    .unwrap_or_default(),
                "workflow": cluster.workflow,
                "match_kind": "exact_key",
                "match_evidence": {
                    "fingerprint": "same_sweep_key",
                    "matched_fields": [{
                        "field": "failure_key",
                        "value": cluster.failure_key,
                    }],
                },
            }));
            continue;
        }
        if filed.len() >= max_tasks {
            skipped_over_cap.push(json!({
                "failure_key": cluster.failure_key,
                "workflow": cluster.workflow,
                "job": cluster.job,
                "run_urls": cluster.run_urls(),
            }));
            continue;
        }

        let task_id = add_task(TaskAddParams {
            title: cluster.title(),
            description: cluster.description(evidence),
            acceptance_criteria: cluster.acceptance_criteria(),
            tags: vec![
                CI_FAILURE_TAG.to_string(),
                format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key),
                "github-actions".to_string(),
            ],
            // Deliberately empty: the evidence is already in the description,
            // so the task ships on the ordinary agent baseline.
            required_tools: Vec::new(),
            crew: system_crew.clone(),
            priority: TaskPriority::High,
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Bug),
            // Filing is quarantine, not dispatch authorization. The
            // task-pilot apply boundary is the only CI-sweep path that may
            // promote a relevant, selector-backed repair to backlog.
            status: Some(TaskStatus::Proposed),
            system_created: true,
            ..TaskAddParams::default()
        })
        .map_err(|error| {
            retryable_pipeline_error(
                "task_creation",
                &audit,
                vec![json!({
                    "stage": "task_creation",
                    "operation": "orbit.task.add",
                    "failure_key": cluster.failure_key,
                    "run_ids": cluster.run_ids(),
                    "retryable": true,
                    "message": bounded_error(&error.to_string()),
                })],
            )
        })?;
        filed_keys.insert(cluster.failure_key.clone());
        let filing = cluster.filing_entry(&task_id);
        pilot_candidate_ids.insert(task_id);
        pilot_candidates.push(filing.clone());
        filed.push(filing);
    }

    let final_audit = filing_audit(audit, &filed, &skipped_existing);
    Ok(json!({
        "outcome": OUTCOME_CURRENT_FAILURES,
        "capability": capability,
        "clusters": clusters.len(),
        "filed_count": filed.len(),
        "filed": filed,
        "pilot_candidate_count": pilot_candidates.len(),
        "pilot_candidates": pilot_candidates,
        "skipped_existing": skipped_existing,
        "repair_assessments": repair_assessments,
        "skipped_over_cap": skipped_over_cap,
        "deferred": deferred,
        "inconclusive": inconclusive,
        "already_repaired": already_repaired,
        "max_tasks": max_tasks,
        "audit": final_audit,
    }))
}

/// The run a snapshot entry — a retryable error or a current failure — is
/// about, as a comparable key. Collection emits a numeric `run_id`; the
/// version-1 `query_errors` shape used a string.
fn run_id_key(entry: &Value) -> Option<String> {
    match entry.get("run_id") {
        Some(Value::Number(number)) => Some(number.to_string()),
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

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

fn split_inconclusive_cancellations(
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
fn drop_inconclusive_log_errors(errors: Vec<Value>, inconclusive: &[Value]) -> Vec<Value> {
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

fn inconclusive_audit(mut audit: Value, inconclusive: &[Value]) -> Value {
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

/// Split retryable errors by blast radius.
///
/// A job error affects only that job; a run error affects all its jobs. An
/// error that names none — a repository read, a run listing, a pull-request
/// listing — leaves the whole snapshot in doubt: any finding it did produce
/// could be missing the newer run that would have superseded it, so filing
/// from that snapshot is not safe.
fn partition_retryable_errors(errors: &[Value]) -> (usize, BTreeMap<String, Vec<Value>>) {
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
fn split_deferred_failures(
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
fn exclude_already_repaired(failures: Vec<Value>, evidence: &Value) -> (Vec<Value>, Vec<Value>) {
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

fn repaired_audit(mut audit: Value, already_repaired: &[Value]) -> Value {
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
fn selected_diagnostic(failure: &Value) -> Option<&str> {
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
fn valid_failure_regions(unit: &Value) -> bool {
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
fn deferred_errors(deferred: &[Value]) -> Vec<Value> {
    deferred
        .iter()
        .filter_map(|entry| entry.get("reasons").and_then(Value::as_array))
        .flat_map(|reasons| reasons.iter().cloned())
        .collect()
}

/// Make partial registration legible: an operator reading the audit must be
/// able to tell "three findings, three filed" from "three findings filed and
/// eleven still owed".
fn deferral_audit(mut audit: Value, deferred: &[Value]) -> Value {
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

fn audit_summary(evidence: &Value, failures: &[Value]) -> Value {
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

fn filing_audit(mut audit: Value, filed: &[Value], skipped_existing: &[Value]) -> Value {
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

fn retryable_pipeline_error(stage: &str, audit: &Value, errors: Vec<Value>) -> OrbitError {
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

fn bounded_error(message: &str) -> String {
    redact_all(message).chars().take(500).collect()
}

fn normalize_retryable_error(error: Value) -> Value {
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

/// One root cause, with every current run that exhibited it.
struct FailureCluster {
    /// Dedupe identity across sweeps. Compiler proof replaces the ordinary
    /// workflow/job/step/signature key only at the same observed checkout.
    failure_key: String,
    /// Grouping identity within one snapshot: `failure_key` plus the commit the
    /// runner actually tested.
    cluster_key: String,
    workflow: String,
    job: String,
    jobs: BTreeSet<String>,
    step: String,
    tested_commit: String,
    signature: String,
    compiler_cause: Option<String>,
    legacy_keys: BTreeSet<String>,
    /// True when `signature` is the failing step name because no error line
    /// survived in the excerpt. The description must label that as a fallback
    /// rather than a captured diagnostic; collapsing every distinct failure of
    /// the step into one `failure_key` is the weaker identity, not a quote.
    signature_is_step_fallback: bool,
    log_excerpt: String,
    failure_region_note: Option<String>,
    log_truncated: bool,
    /// The job whose own log supplied the excerpt, when the run-scoped read
    /// returned nothing and collection recovered it per job. Such an excerpt is
    /// the whole job's log, not just its failed steps, and the description says
    /// so rather than presenting it as a failed-step quote.
    log_source_job: Option<String>,
    runs: Vec<Value>,
}

impl FailureCluster {
    fn find_covering_task<L: DuplicateTaskLookup + ?Sized>(
        &self,
        lookup: &L,
    ) -> Result<Option<DuplicateTaskMatch>, OrbitError> {
        if let Some(found) = find_covering_task(lookup, &self.duplicate_candidate())? {
            return Ok(Some(found));
        }
        // Shipped per-job keys (including the old first-marker signature) are
        // durable references. Keep their exact/rejected-owner continuity, but
        // never use their weak command or source-only fingerprints as proof.
        for key in &self.legacy_keys {
            let tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{key}");
            let candidate = DuplicateCandidate::new(
                tag.clone(),
                vec![CoverageFingerprint::new(
                    "legacy_compiler_key",
                    vec![CoverageAnchor::new("exact_failure_key", tag)],
                )],
            );
            if let Some(found) = find_covering_task(lookup, &candidate)? {
                let source_id = found.evidence["matched_fields"]
                    .as_array()
                    .and_then(|fields| {
                        fields
                            .iter()
                            .find(|field| field["field"] == "rejected_task_id")
                    })
                    .and_then(|field| field["value"].as_str())
                    .unwrap_or(&found.task_id);
                let source = lookup.get_task(source_id)?;
                let owner = lookup.get_task(&found.task_id)?;
                let prior_cause = compiler_cause(&source.description)
                    .or_else(|| compiler_cause(&owner.description));
                if prior_cause.is_some() && prior_cause != self.compiler_cause {
                    continue;
                }
                // An old chatter-based key can recur for a different compiler
                // cause. Only immutable supplying evidence justifies migration.
                if self
                    .runs
                    .iter()
                    .any(|run| legacy_source_matches(&source.description, run))
                {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    fn duplicate_candidate(&self) -> DuplicateCandidate {
        let exact_tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", self.failure_key);
        if let Some(cause) = &self.compiler_cause {
            let mut fingerprints = vec![CoverageFingerprint::new(
                "ci_compiler_cause",
                vec![
                    CoverageAnchor::new("compiler_cause", digest(&[cause])),
                    CoverageAnchor::new("tested_commit", &self.tested_commit),
                ],
            )];
            fingerprints.extend(self.provenance_fingerprints());
            return DuplicateCandidate::new(exact_tag, fingerprints)
                .with_completed_fingerprints(self.completed_provenance_fingerprints());
        }
        let mut fingerprints = if self.signature_is_step_fallback {
            // A step-name fallback contains no diagnostic. It is sufficient
            // for exact-key idempotency but too weak for broader free-text
            // coverage, where it could suppress an unrelated failure of the
            // same generic CI step.
            vec![CoverageFingerprint::new(
                "ci_failure_unmatchable_fallback",
                vec![CoverageAnchor::new("exact_failure_key", &exact_tag)],
            )]
        } else {
            vec![CoverageFingerprint::new(
                "ci_failure_root_cause",
                vec![
                    CoverageAnchor::new("workflow", format!("workflow {}", self.workflow)),
                    CoverageAnchor::new("job", format!("failing job {}", self.job)),
                    CoverageAnchor::new("step", format!("failing step {}", self.step)),
                    CoverageAnchor::new("normalized_error_signature", &self.signature),
                ],
            )]
        };
        if !self.signature_is_step_fallback {
            if let Some(command) = specific_command_from_log(&self.log_excerpt) {
                for diagnostic in specific_error_anchors(&self.log_excerpt, &self.signature)
                    .into_iter()
                    .take(3)
                {
                    fingerprints.push(CoverageFingerprint::new(
                        "ci_failure_error_and_command",
                        vec![
                            CoverageAnchor::new("specific_error", diagnostic),
                            CoverageAnchor::new("command", command.clone()),
                        ],
                    ));
                }
            }
            if let Some(fingerprint) = source_identity_fingerprint(&self.runs) {
                fingerprints.push(fingerprint);
            }
        }
        fingerprints.extend(self.provenance_fingerprints());
        DuplicateCandidate::new(exact_tag, fingerprints)
            .with_completed_fingerprints(self.completed_provenance_fingerprints())
    }

    fn provenance_fingerprints(&self) -> Vec<CoverageFingerprint> {
        self.provenance_fingerprints_with_test_names(true)
    }

    fn completed_provenance_fingerprints(&self) -> Vec<CoverageFingerprint> {
        self.provenance_fingerprints_with_test_names(false)
    }

    fn provenance_fingerprints_with_test_names(
        &self,
        include_test_names: bool,
    ) -> Vec<CoverageFingerprint> {
        let mut fingerprints = Vec::new();
        let mut seen = BTreeSet::new();
        for run in &self.runs {
            let run_id = value_string(run, "run_id");
            if !run_id.is_empty() && seen.insert(("run_id", run_id.clone())) {
                fingerprints.push(CoverageFingerprint::new(
                    "ci_failure_run_id",
                    vec![CoverageAnchor::new("run_id", run_id)],
                ));
            }
            for sha in [
                value_string(run, "event_reported_head_sha"),
                value_string(run, "current_ref_head_sha"),
                tested_commit(run),
            ] {
                if !sha.is_empty() && seen.insert(("head_sha", sha.clone())) {
                    fingerprints.push(CoverageFingerprint::new(
                        "ci_failure_head_sha",
                        vec![CoverageAnchor::new("head_sha", sha)],
                    ));
                }
            }
            if include_test_names {
                for name in failure_test_names(run) {
                    if seen.insert(("test_name", name.clone())) {
                        fingerprints.push(CoverageFingerprint::new(
                            "ci_failure_test_name",
                            vec![CoverageAnchor::new("test_name", name)],
                        ));
                    }
                }
            }
        }
        fingerprints
    }

    fn run_urls(&self) -> Vec<String> {
        self.runs
            .iter()
            .filter_map(|run| run.get("url").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect()
    }

    fn run_ids(&self) -> Vec<Value> {
        self.runs
            .iter()
            .filter_map(|run| run.get("run_id").cloned())
            .collect()
    }

    fn filing_entry(&self, task_id: &str) -> Value {
        json!({
            "task_id": task_id,
            "failure_key": self.failure_key,
            "cluster_key": self.cluster_key,
            "workflow": self.workflow,
            "job": self.job,
            "step": self.step,
            "jobs": self.jobs,
            "tested_commit": self.tested_commit,
            "sources": self.runs.iter().map(|run| json!({
                "run_id": run["run_id"], "job_id": run["job_id"],
                "workflow": run["workflow"], "failed_jobs": run["failed_jobs"],
                "actual_checkout_shas": run["actual_checkout_shas"],
            })).collect::<Vec<_>>(),
            "run_ids": self.run_ids(),
            "run_urls": self.run_urls(),
            "ref_kinds": self.distinct_run_strings("ref_kind"),
            "head_branches": self.distinct_run_strings("head_branch"),
        })
    }

    fn distinct_run_strings(&self, field: &str) -> Vec<String> {
        self.runs
            .iter()
            .filter_map(|run| run.get(field).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn title(&self) -> String {
        let where_ = match (self.job.as_str(), self.step.as_str()) {
            ("", "") => self.workflow.clone(),
            (job, "") => format!("{} / {job}", self.workflow),
            ("", step) => format!("{} / {step}", self.workflow),
            (job, step) => format!("{} / {job} / {step}", self.workflow),
        };
        // Cap the body, not the whole string, so a long workflow/job/step
        // never eats into the prefix and the final title still respects the
        // existing 120-character bound.
        let body_budget = 120usize.saturating_sub(CI_FAILURE_SWEEP_TITLE_PREFIX.chars().count());
        let body = truncate_chars(&format!("Fix red CI: {where_}"), body_budget);
        format!("{CI_FAILURE_SWEEP_TITLE_PREFIX}{body}")
    }

    fn acceptance_criteria(&self) -> Vec<String> {
        vec![
            format!(
                "The root cause of the `{}` failure recorded below is fixed in this repository; \
                 the workflow, assertion, or lint level is not disabled, weakened, or made \
                 non-blocking to obtain green.",
                self.workflow
            ),
            "The exact command or narrowest faithful local equivalent that failed on the runner \
             is reproduced and then passes locally."
                .to_string(),
            "The repository's documented pre-handoff gate passes.".to_string(),
            "This task carries a `regression_from` relation targeting the task whose landed \
             commit introduced the failure, or its execution summary states why no task can \
             be held responsible (no task ID on the culprit commit, or the failure is \
             infrastructure rather than repository-owned)."
                .to_string(),
            "If the failure turns out to be infrastructure rather than repository-owned, the \
             execution summary cites concrete evidence for that (a same-commit retry that \
             succeeded, or runner/service fault output) rather than a single non-reproduction."
                .to_string(),
        ]
    }

    /// Render the evidence a remediation agent needs, inline.
    ///
    /// This is the whole point of the sweep: the agent that picks this task up
    /// cannot reach GitHub, so anything absent here is unavailable to it.
    fn description(&self, evidence: &Value) -> String {
        let mut out = String::new();
        out.push_str(
            "This task was filed automatically from a host-side sweep of this repository's \
             GitHub Actions runs. Every CI query ran on the host before this task existed, so \
             the evidence below is all of it — the execution lane for this task cannot reach \
             GitHub, and it is not expected to.\n\n",
        );

        out.push_str("## Failure\n\n");
        out.push_str(&format!("- Workflow: `{}`\n", display(&self.workflow)));
        out.push_str(&format!("- Failing job: `{}`\n", display(&self.job)));
        if self.jobs.len() > 1 {
            let additional = self
                .jobs
                .iter()
                .filter(|job| *job != &self.job)
                .map(|job| format!("`{}`", display(job)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "- Other failing jobs in this test cluster: {additional}\n"
            ));
        }
        out.push_str(&format!("- Failing step: `{}`\n", display(&self.step)));
        out.push_str(&format!(
            "- Commit the runner actually checked out: `{}`\n",
            display(&self.tested_commit)
        ));
        if let Some(cause) = &self.compiler_cause {
            out.push_str(&format!(
                "- Compiler cause identity: `{}`\n",
                digest(&[cause])
            ));
        }
        if self.signature_is_step_fallback {
            out.push_str(&format!(
                "- Normalized error signature (step-name fallback — no error line was captured; \
                 the dedupe identity, not a quote): `{}`\n",
                display(&self.signature)
            ));
        } else if self.compiler_cause.is_some() {
            out.push_str(&format!(
                "- Normalized error signature (display only; the compiler cause identity controls dedupe): `{}`\n",
                display(&self.signature)
            ));
        } else {
            out.push_str(&format!(
                "- Normalized error signature (the dedupe identity, not a quote): `{}`\n",
                display(&self.signature)
            ));
        }
        if let Some(repository) = evidence.get("repository").and_then(Value::as_object) {
            if let Some(full_name) = repository.get("full_name").and_then(Value::as_str) {
                out.push_str(&format!("- Repository: `{full_name}`\n"));
            }
            if let Some(default_branch) = repository.get("default_branch").and_then(Value::as_str) {
                out.push_str(&format!(
                    "- Release branch as GitHub reports it: `{default_branch}`\n"
                ));
            }
        }
        if let Some(collected_at) = evidence.get("collected_at").and_then(Value::as_str) {
            out.push_str(&format!("- Evidence collected at: {collected_at}\n"));
        }

        out.push_str("\n## Runs exhibiting this failure\n\n");
        out.push_str(
            "Three commits are routinely conflated and are kept apart here: the SHA the \
             workflow event reported, the SHA the ref points at now, and the commit the runner \
             actually checked out. A pull-request merge SHA is not a pull-request head SHA.\n\n",
        );
        for run in self.runs.iter().take(MAX_LISTED_RUNS) {
            out.push_str(&render_run(run));
        }
        if self.runs.len() > MAX_LISTED_RUNS {
            out.push_str(&format!(
                "\n_{} further run(s) in this cluster are not listed._\n",
                self.runs.len() - MAX_LISTED_RUNS
            ));
        }

        out.push_str("\n## Failed-step log excerpt\n\n");
        if self.log_excerpt.trim().is_empty() {
            out.push_str(
                "_No log excerpt was captured for this cluster. Reproduce the failing job's \
                 command locally instead of guessing from the step name._\n",
            );
            let relevant = relevant_log_query_errors(evidence, &self.runs);
            if !relevant.is_empty() {
                out.push('\n');
                for error in relevant {
                    out.push_str(&format!(
                        "- `{}` for run `{}`: {}\n",
                        display(&value_string(error, "query")),
                        display(&value_string(error, "run_id")),
                        truncate_chars(&value_string(error, "error"), 300),
                    ));
                }
            }
        } else {
            let excerpt = if let Some(note) = &self.failure_region_note {
                out.push_str(note);
                // Already capped at 64 KiB by collection and checked again at
                // filing. Keep every selected failure for the offline worker.
                FailedStepExcerpt {
                    body: self.log_excerpt.clone(),
                    has_anchor: true,
                }
            } else {
                render_failed_step_excerpt(&self.log_excerpt, DESCRIPTION_LOG_BYTES)
            };
            if !excerpt.body.trim().is_empty() {
                out.push_str("```\n");
                out.push_str(&excerpt.body);
                out.push_str("\n```\n");
            }
            if !excerpt.has_anchor {
                out.push_str(
                    "\n_No error anchor was present in the retained excerpt; the env/with dump \
                     is omitted rather than shown as evidence._\n",
                );
            }
            if self.log_truncated {
                out.push_str(
                    "\n_The collection display was truncated. Selected evidence is used for diagnosis; \
                     its retention limits are independent of that display._\n",
                );
            }
            if let Some(job) = &self.log_source_job {
                out.push_str(&format!(
                    "\n_The run-scoped failed-step log came back empty, so this excerpt is the \
                     evidence from job {job}, read from the job log API._\n"
                ));
            }
        }

        let stale = self.stale_evidence(evidence);
        if !stale.is_empty() {
            out.push_str("\n## Stale or superseded runs of this workflow\n\n");
            out.push_str(
                "These are already excluded from the failure above. They are listed so the \
                 repair is not attributed to a run that no longer reflects the current head.\n\n",
            );
            for entry in &stale {
                out.push_str(&format!(
                    "- `{}` run {} — {}: {}\n",
                    display(&value_string(entry, "workflow")),
                    display(&value_string(entry, "url")),
                    display(&value_string(entry, "reason")),
                    display(&value_string(entry, "evidence")),
                ));
            }
        }

        if let Some(truncation) = evidence.get("truncation") {
            out.push_str("\n## Collection bounds\n\n");
            out.push_str(
                "Reported so \"no more failures\" is never mistaken for \"we stopped \
                 looking\".\n\n",
            );
            out.push_str("```json\n");
            out.push_str(&truncate_bytes(
                &serde_json::to_string_pretty(truncation).unwrap_or_default(),
                2_000,
            ));
            out.push_str("\n```\n");
        }

        let query_errors = evidence
            .get("query_errors")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !query_errors.is_empty() {
            out.push_str("\n## Queries that failed during collection\n\n");
            for error in query_errors {
                out.push_str(&format!("- {}\n", truncate_chars(&error.to_string(), 300)));
            }
        }

        out.push_str(
            "\n## How to finish\n\n\
             Reproduce the failure at the current repository head using the exact command or \
             the narrowest faithful local equivalent, fix the repository-owned cause, and rerun \
             that command. Do not disable a workflow, weaken an assertion or lint level, add a \
             broad allow rule, or mark a failing gate non-blocking. Then run the repository's \
             documented pre-handoff gate. Verification happens on this task's own pull request: \
             CI runs there normally, and if the failure is still current the next sweep will see \
             it again.\n\
             \n\
             ## Attribute the regression\n\
             \n\
             Identify the task whose landed change introduced this failure and record it on \
             this task. Start from the commit the runner checked out and walk back to the \
             newest commit at which the failing command last passed; the commit that broke it \
             is the culprit. Read the task ID from that commit — squash-merge subjects carry \
             it as `[<task-id>]` — and confirm with `orbit tool run orbit.task.show --input \
             '{\"id\":\"<task-id>\"}'` that its change is the one at fault, not a later commit \
             that merely touched the same file. Then set the relation with `orbit tool run \
             orbit.task.update --input '{\"id\":\"<this task ID>\",\"relations\":[<existing \
             relations from orbit.task.show>,{\"type\":\"regression_from\",\"target\":\"<culprit \
             task ID>\"}]}'` — `relations` replaces the whole list, so carry the existing \
             entries forward. When the culprit commit carries no task ID, or the failure is \
             infrastructure rather than repository-owned, record that conclusion and its \
             evidence in the execution summary instead of blaming a bystander task.\n",
        );
        out
    }

    /// Stale/superseded runs of the same workflow, so the description can say
    /// why they were excluded rather than leaving them unexplained.
    fn stale_evidence(&self, evidence: &Value) -> Vec<Value> {
        evidence
            .get("stale_or_superseded")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| value_string(entry, "workflow") == self.workflow)
                    .take(MAX_LISTED_RUNS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The job whose own log supplied this failure's excerpt, if the run-scoped
/// read produced nothing and collection fell back per job.
fn job_log_source(failure: &Value) -> Option<String> {
    if value_string(failure, "log_source") != "job_api_log" {
        return None;
    }
    let job = failure.get("log_source_jobs")?.as_array()?.first()?;
    Some(format!(
        "`{}` (id `{}`)",
        display(&value_string(job, "name")),
        display(&value_string(job, "job_id")),
    ))
}

fn render_run(run: &Value) -> String {
    let mut out = format!(
        "- {} run `{}` ({} on `{}`) — status `{}`, conclusion `{}`\n",
        display(&value_string(run, "url")),
        display(&value_string(run, "run_id")),
        display(&value_string(run, "event")),
        display(&value_string(run, "head_branch")),
        display(&value_string(run, "status")),
        display(&value_string(run, "conclusion")),
    );
    out.push_str(&format!(
        "  - event-reported head SHA: `{}`\n",
        display(&value_string(run, "event_reported_head_sha"))
    ));
    out.push_str(&format!(
        "  - current head of that ref: `{}`\n",
        display(&value_string(run, "current_ref_head_sha"))
    ));
    let checkout = run
        .get("actual_checkout_shas")
        .and_then(Value::as_array)
        .map(|shas| {
            shas.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    out.push_str(&format!(
        "  - commit actually checked out: `{}`\n",
        display(&checkout)
    ));
    if let Some(pr) = run.get("pr_number").and_then(Value::as_u64) {
        out.push_str(&format!("  - pull request: #{pr}\n"));
    }
    for line in run
        .get("checkout_evidence")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .take(3)
    {
        out.push_str(&format!(
            "  - checkout evidence: `{}`\n",
            truncate_chars(line, 200)
        ));
    }
    for job in run
        .get("failed_jobs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(3)
    {
        out.push_str(&format!(
            "  - failed job `{}` (id `{}`): {}\n",
            display(&value_string(job, "name")),
            display(&value_string(job, "job_id")),
            display(&value_string(job, "url")),
        ));
        let steps = job
            .get("failed_steps")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|step| display(&value_string(step, "name")).to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        if !steps.is_empty() {
            out.push_str(&format!("  - failing step(s): `{steps}`\n"));
        }
    }
    out
}

/// Group current failures by root cause.
///
/// Preserves the order collection chose (integration head first, then release,
/// then pull requests), so the filing cap spends itself on the heads that gate
/// delivery.
fn cluster_failures(failures: &[Value]) -> Vec<FailureCluster> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: BTreeMap<String, FailureCluster> = BTreeMap::new();

    for failure in failures {
        // A listed-but-uninvestigated failure carries no job, step, or log —
        // only a run URL. Filing a task from it would produce a task whose
        // evidence section is empty, which is worse than reporting it as an
        // unfiled bound. `truncation` already names how many there were.
        if failure.get("investigated").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let workflow = value_string(failure, "workflow");
        let (job, step) = failing_job_and_step(failure);
        let log_excerpt = selected_diagnostic(failure)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value_string(failure, "log_excerpt"));
        let signature = error_signature(&log_excerpt, &step);
        let test_names = failure_test_names(failure);
        let test_identity = test_names.join("\u{1f}");
        let grouping_identity = if test_identity.is_empty() {
            format!("signature:{}", signature.text)
        } else {
            format!("test:{test_identity}")
        };
        let tested_commit = tested_commit(failure);

        let regions = valid_failure_regions(&failure["diagnostic_unit"]);
        // Partial command retention cannot prove an exhaustive compiler set.
        let compiler_cause = (!regions).then(|| compiler_cause(&log_excerpt)).flatten();
        let legacy_key = compiler_cause.as_ref().map(|_| {
            let lines = classify_log_lines(&log_excerpt);
            let legacy = legacy_signature(&lines, &step);
            digest(&[&workflow, &job, &step, &legacy])
        });
        // Cross-job consolidation requires the complete compiler diagnostic
        // set, exact source locations and the same observed checkout. Generic
        // step wrappers and shared paths are never sufficient.
        let failure_key = match &compiler_cause {
            Some(cause) => digest(&["compiler", cause, &tested_commit]),
            None => digest(&[&workflow, &job, &step, &signature.text]),
        };
        let cluster_key = if compiler_cause.is_some() {
            digest(&[&failure_key, &tested_commit])
        } else {
            digest(&[&workflow, &step, &grouping_identity, &tested_commit])
        };

        let cluster = grouped.entry(cluster_key.clone()).or_insert_with(|| {
            order.push(cluster_key.clone());
            FailureCluster {
                failure_key,
                cluster_key: cluster_key.clone(),
                workflow,
                job: job.clone(),
                jobs: BTreeSet::new(),
                step,
                tested_commit,
                signature: signature.text,
                compiler_cause,
                legacy_keys: BTreeSet::new(),
                signature_is_step_fallback: signature.step_fallback,
                log_excerpt,
                failure_region_note: regions.then(|| {
                    let unit = &failure["diagnostic_unit"];
                    format!(
                        "_Failure regions from a completely scanned command; the full command was not retained. {} of {} source bytes omitted, including {} assertion payload bytes. All {} recognized failure anchors and bounded context are retained (64 KiB selection limit)._\n\n",
                        unit["omitted_bytes"], unit["command_bytes"],
                        unit["assertion_payload_omitted_bytes"], unit["failure_anchor_count"],
                    )
                }),
                log_truncated: failure
                    .get("log_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                log_source_job: job_log_source(failure),
                runs: Vec::new(),
            }
        });
        if !job.is_empty() {
            cluster.jobs.insert(job);
        }
        if let Some(key) = legacy_key {
            cluster.legacy_keys.insert(key);
        }
        cluster.runs.push(failure.clone());
    }

    order
        .into_iter()
        .filter_map(|key| grouped.remove(&key))
        .collect()
}

/// The single job and step whose evidence passed registration.
fn failing_job_and_step(failure: &Value) -> (String, String) {
    let Some(job) = failure
        .get("failed_jobs")
        .and_then(Value::as_array)
        .and_then(|jobs| jobs.first())
    else {
        return (String::new(), String::new());
    };
    let step = job
        .get("failed_steps")
        .and_then(Value::as_array)
        .and_then(|steps| steps.first())
        .map(|step| value_string(step, "name"))
        .unwrap_or_default();
    (value_string(job, "name"), step)
}

/// The observed checkout, validated before clustering. Event and PR heads
/// are never substitutes for the commit the supplying job tested.
fn tested_commit(failure: &Value) -> String {
    failure
        .get("actual_checkout_shas")
        .and_then(Value::as_array)
        .and_then(|shas| shas.first())
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default()
}

/// Extract stable test identities from the diagnostic or an explicit
/// collector field. These are intentionally separate from the normalized
/// error signature: a manual task may name the test without copying the CI
/// wrapper labels or the exact diagnostic wording.
fn failure_test_names(failure: &Value) -> Vec<String> {
    let mut names = BTreeSet::new();
    for key in ["test_name", "failing_test", "failed_test"] {
        if let Some(name) = failure.get(key).and_then(Value::as_str)
            && !name.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
    }
    for key in ["test_names", "failing_tests", "failed_tests"] {
        if let Some(values) = failure.get(key).and_then(Value::as_array) {
            names.extend(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
    }

    let log = {
        let excerpt = value_string(failure, "log_excerpt");
        if excerpt.is_empty() {
            value_string(&failure["diagnostic_unit"], "text")
        } else {
            excerpt
        }
    };
    let mut after_failures_header = false;
    for line in log.lines() {
        let payload = signature_payload(line);
        let line = payload.trim().to_string();
        if line == "failures:" || line == "errors:" {
            after_failures_header = true;
            continue;
        }
        if is_libtest_stdout_header(&line) || line.starts_with("test result:") {
            after_failures_header = false;
        }
        if let Some(rest) = line.strip_prefix("test ")
            && let Some((name, suffix)) = rest.split_once(" ... ")
            && matches!(suffix.split_whitespace().next(), Some("failed"))
            && !name.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("thread '")
            && let Some((name, suffix)) = rest.split_once("' panicked")
            && !name.trim().is_empty()
            && !suffix.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("thread \"")
            && let Some((name, suffix)) = rest.split_once("\" panicked")
            && !name.trim().is_empty()
            && !suffix.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("fail [")
            && let Some((_, name)) = rest.split_once(']')
            && !name.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if after_failures_header
            && (payload.starts_with(' ') || payload.starts_with('\t'))
            && !line.contains(' ')
            && !line.starts_with("----")
            && !line.starts_with("thread")
            && !line.starts_with("error")
            && !line.starts_with("assertion")
        {
            names.insert(line.trim().to_string());
        }
    }
    names.into_iter().collect()
}

/// Generated descriptions shipped these exact provenance labels. This is a
/// compatibility check for old tags, not a free-text root-cause heuristic.
fn legacy_source_matches(description: &str, run: &Value) -> bool {
    let run_id = value_string(run, "run_id");
    let job_id = value_string(run, "job_id");
    let checkout = tested_commit(run);
    !run_id.is_empty()
        && !job_id.is_empty()
        && !checkout.is_empty()
        && description.contains(&format!("run `{run_id}`"))
        && description.contains(&format!("(id `{job_id}`)"))
        && description.contains(&format!("commit actually checked out: `{checkout}`"))
}

fn source_identity_fingerprint(runs: &[Value]) -> Option<CoverageFingerprint> {
    let run = runs.first()?;
    let run_id = value_string(run, "run_id");
    let job_id = value_string(run, "job_id");
    if run_id.is_empty() || job_id.is_empty() {
        return None;
    }
    Some(CoverageFingerprint::new(
        "ci_failure_source_identity",
        vec![
            CoverageAnchor::new("run_id", run_id),
            CoverageAnchor::new("job_id", job_id),
        ],
    ))
}

/// Distinctive diagnostic lines a manual repair brief can quote without
/// generated `workflow` / `failing job` / `failing step` labels.
fn specific_error_anchors(log: &str, signature: &str) -> Vec<String> {
    let mut anchors: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let Some(normalized) = specific_error_text(value) else {
            return;
        };
        if !anchors
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&normalized))
        {
            anchors.push(normalized);
        }
    };
    push(signature);
    for (kind, line) in classify_log_lines(log) {
        if !matches!(
            kind,
            LineKind::CompilerDiagnostic
                | LineKind::ConcreteDiagnostic
                | LineKind::ErrorAnnotated
                | LineKind::Marker
                | LineKind::Content
        ) {
            continue;
        }
        push(&strip_ansi_sequences(log_payload(line)));
    }
    anchors
}

fn specific_error_text(value: &str) -> Option<String> {
    let trimmed = value
        .trim()
        .trim_start_matches(|character: char| {
            character == '-' || character == '*' || character.is_whitespace()
        })
        .trim();
    if trimmed.chars().count() < 16 || trimmed.chars().count() > 180 {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if is_generic_trailer(&lowered)
        || is_run_command_payload(&lowered)
        || lowered.starts_with("[command]")
        || (lowered.contains("added ") && lowered.contains("packages"))
        || lowered.contains("looking for funding")
        || lowered.contains("found 0 vulnerabilities")
        || lowered.contains("wrangler installed")
        || lowered.contains("logs were written")
    {
        return None;
    }
    let diagnostic = ERROR_MARKERS.iter().any(|marker| lowered.contains(marker))
        || lowered.contains("missing")
        || lowered.contains("not found")
        || lowered.contains("cannot")
        || lowered.contains("invalid")
        || lowered.contains("expected")
        || lowered.contains("required");
    diagnostic.then(|| trimmed.to_string())
}

fn specific_command_from_log(log: &str) -> Option<String> {
    let mut best = None;
    for (kind, line) in classify_log_lines(log) {
        let payload = log_payload(line);
        let raw = if kind == LineKind::RunCommand {
            run_command_body(payload)
        } else {
            bracket_command_body(payload)
        };
        let Some(raw) = raw else {
            continue;
        };
        if let Some(stable) = stabilize_command(raw) {
            best = Some(stable);
        }
    }
    best
}

fn run_command_body(payload: &str) -> Option<&str> {
    let trimmed = payload.trim();
    let lower = trimmed.to_ascii_lowercase();
    const PREFIX: &str = "##[group]run ";
    lower
        .starts_with(PREFIX)
        .then(|| trimmed.get(PREFIX.len()..).unwrap_or_default().trim())
        .filter(|body| !body.is_empty())
}

fn bracket_command_body(payload: &str) -> Option<&str> {
    let trimmed = payload.trim();
    let lower = trimmed.to_ascii_lowercase();
    const PREFIX: &str = "[command]";
    lower
        .starts_with(PREFIX)
        .then(|| trimmed.get(PREFIX.len()..).unwrap_or_default().trim())
        .filter(|body| !body.is_empty())
}

fn stabilize_command(raw: &str) -> Option<String> {
    let mut tokens = raw.split_whitespace().collect::<Vec<_>>();
    if tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("[command]"))
    {
        tokens.remove(0);
    }
    while tokens
        .first()
        .is_some_and(|token| is_javascript_runtime_token(token))
    {
        tokens.remove(0);
        if tokens.first().is_some_and(|token| {
            matches!(*token, "--no-install" | "--yes" | "-y" | "--prefer-offline")
        }) {
            tokens.remove(0);
        }
    }
    if tokens.is_empty() || is_install_invocation(&tokens) {
        return None;
    }
    let mut kept = Vec::new();
    let mut skip_value = false;
    for token in tokens {
        if skip_value {
            skip_value = false;
            continue;
        }
        if is_volatile_command_flag(token) {
            if !token.contains('=') {
                skip_value = true;
            }
            continue;
        }
        kept.push(token);
    }
    if is_generic_command(&kept) {
        return None;
    }
    Some(kept.join(" "))
}

fn is_javascript_runtime_token(token: &str) -> bool {
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase();
    matches!(base.as_str(), "npx" | "npm" | "node" | "yarn" | "pnpm")
}

fn is_install_invocation(tokens: &[&str]) -> bool {
    matches!(
        tokens
            .first()
            .map(|token| token.to_ascii_lowercase())
            .as_deref(),
        Some("i" | "install" | "add" | "ci")
    )
}

fn is_volatile_command_flag(token: &str) -> bool {
    let name = token
        .trim_start_matches('-')
        .split_once('=')
        .map(|(name, _)| name)
        .unwrap_or_else(|| token.trim_start_matches('-'));
    matches!(
        name,
        "commit-hash" | "commit-message" | "commit" | "sha" | "hash"
    )
}

fn is_generic_command(tokens: &[&str]) -> bool {
    if tokens.is_empty() {
        return true;
    }
    let joined = tokens.join(" ").to_ascii_lowercase();
    if matches!(
        joined.as_str(),
        "cargo test"
            | "cargo build"
            | "cargo check"
            | "cargo clippy"
            | "cargo nextest run"
            | "cargo llvm-cov"
            | "npm test"
            | "npm run build"
            | "yarn test"
            | "pnpm test"
            | "npx"
            | "node"
            | "wrangler --version"
            | "wrangler version"
    ) {
        return true;
    }
    let distinctive = tokens.iter().any(|token| {
        token.contains('/')
            || token.contains('\\')
            || (token.starts_with("--") && token.contains('='))
            || token.contains('@')
    });
    tokens.len() < 3 && !distinctive
}

/// Lines a runner emits when something breaks.
const ERROR_MARKERS: &[&str] = &[
    "error",
    "failed",
    "failure",
    "panicked",
    "assertion",
    "exit code",
    "not ok",
    "fatal",
];

/// Reduce a failed-step log to one normalized line that survives a rerun.
///
/// Reruns of the same regression differ in timestamps, durations, run numbers,
/// ANSI styling, and paths under a run-specific temp directory. Normalizing
/// those away is what lets an hourly sweep recognize the same root cause
/// instead of filing it again every hour.
///
/// Preference order, so a generic wrapper cannot fragment one evidenced
/// failure or collapse distinct ones:
/// 1. A coded compiler diagnostic (`error[E0062]: …`).
/// 2. A concrete test/panic identity (`thread '…' panicked`, `test … FAILED`,
///    nextest `FAIL […]`, a name listed after libtest `failures:`).
/// 3. A specific `##[error]` annotation.
/// 4. Any remaining marker diagnostic (compiler `error:`, `assertion failed`).
/// 5. The nearest unannotated content line before a generic trailer.
/// 6. The failing step name, labelled as a fallback — used when the excerpt
///    only has wrappers, bookkeeping, or assertion payload.
///
/// Generic trailers include GitHub's process-completed / `The process '…'
/// failed with exit code` / action-failed annotations, cargo's
/// `test failed, to rerun pass` wrappers, and nextest cancellation/summary
/// lines. Assertion `left:`/`right:` dumps are not signatures even when they
/// contain marker words. Raw excerpt bytes stay in the filed description;
/// ANSI is stripped only for classification and the normalized signature.
fn error_signature(log_excerpt: &str, step: &str) -> ErrorSignature {
    let lines = classify_log_lines(log_excerpt);
    if let Some(index) = diagnostic_anchor(&lines) {
        return ErrorSignature {
            text: normalize_signature(&signature_payload(lines[index].1)),
            step_fallback: false,
        };
    }
    ErrorSignature {
        text: normalize_signature(&step.to_ascii_lowercase()),
        step_fallback: true,
    }
}

/// Signature and display must agree on the strongest diagnostic, independent
/// of where setup output or a process-exit wrapper appears in the command.
fn diagnostic_anchor(lines: &[(LineKind, &str)]) -> Option<usize> {
    for wanted in [
        LineKind::CompilerDiagnostic,
        LineKind::ConcreteDiagnostic,
        LineKind::ErrorAnnotated,
        LineKind::Marker,
    ] {
        if let Some(index) = lines.iter().position(|(kind, _)| *kind == wanted) {
            return Some(index);
        }
    }
    for (index, (kind, _)) in lines.iter().enumerate() {
        if *kind == LineKind::GenericTrailer
            && let Some(previous) = lines[..index]
                .iter()
                .rposition(|(kind, line)| *kind == LineKind::Content && is_diagnostic_content(line))
        {
            return Some(previous);
        }
    }
    None
}

/// Preserve the shipped signature algorithm only for looking up existing tags.
fn legacy_signature(lines: &[(LineKind, &str)], step: &str) -> String {
    let legacy: Vec<_> = lines
        .iter()
        .map(|(kind, line)| {
            let kind = match kind {
                LineKind::CompilerDiagnostic | LineKind::CargoStatus => {
                    if signature_payload(line).contains("##[error]") {
                        LineKind::ErrorAnnotated
                    } else if is_error_marker_line(signature_payload(line).trim()) {
                        LineKind::Marker
                    } else {
                        LineKind::Content
                    }
                }
                other => *other,
            };
            (kind, *line)
        })
        .collect();
    diagnostic_anchor(&legacy)
        .map(|index| normalize_signature(&signature_payload(legacy[index].1)))
        .unwrap_or_else(|| normalize_signature(&step.to_ascii_lowercase()))
}

fn is_compiler_diagnostic(payload: &str) -> bool {
    let payload = payload.strip_prefix("##[error]").unwrap_or(payload).trim();
    let Some(rest) = payload.strip_prefix("error[e") else {
        return false;
    };
    let Some((code, message)) = rest.split_once("]:") else {
        return false;
    };
    code.len() == 4 && code.bytes().all(|byte| byte.is_ascii_digit()) && !message.trim().is_empty()
}

fn is_cargo_status(payload: &str) -> bool {
    [
        "compiling ",
        "checking ",
        "downloading ",
        "downloaded ",
        "fresh ",
        "error: could not compile ",
        "warning: build failed",
        "for more information about this error",
    ]
    .iter()
    .any(|prefix| payload.starts_with(prefix))
}

/// A conservative proof for cross-job merging. Preserve diagnostic operands
/// and line/column numbers: display normalization deliberately erases numbers
/// and truncates text, so it is not strong enough for a compiler cause key.
fn compiler_cause(log: &str) -> Option<String> {
    let lines = classify_log_lines(log);
    let mut causes = BTreeSet::new();
    for (index, (kind, line)) in lines.iter().enumerate() {
        if *kind != LineKind::CompilerDiagnostic {
            continue;
        }
        let diagnostic = strip_ansi_sequences(log_payload(line));
        let diagnostic = diagnostic
            .trim()
            .strip_prefix("##[error]")
            .unwrap_or(diagnostic.trim())
            .trim();
        let location = lines
            .get(index + 1)
            .map(|(_, line)| strip_ansi_sequences(log_payload(line)))?;
        let location = location.trim().strip_prefix("-->")?.trim();
        let (path_line, column) = location.rsplit_once(':')?;
        let (path, line_number) = path_line.rsplit_once(':')?;
        if path.starts_with('/')
            || path.contains("..")
            || !path.ends_with(".rs")
            || line_number.parse::<u64>().is_err()
            || column.parse::<u64>().is_err()
        {
            return None;
        }
        causes.insert(format!("{diagnostic} @ {location}"));
    }
    (!causes.is_empty()).then(|| causes.into_iter().collect::<Vec<_>>().join("; "))
}

struct ErrorSignature {
    text: String,
    step_fallback: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    RunCommand,
    ParamDump,
    EndGroup,
    CompilerDiagnostic,
    CargoStatus,
    ConcreteDiagnostic,
    ErrorAnnotated,
    GenericTrailer,
    Marker,
    Bookkeeping,
    Content,
}

impl LineKind {
    fn skip_from_excerpt(self) -> bool {
        matches!(
            self,
            Self::ParamDump | Self::EndGroup | Self::Bookkeeping | Self::CargoStatus
        )
    }
}

struct FailedStepExcerpt {
    body: String,
    has_anchor: bool,
}

/// Command line plus the failure region, never a head-biased env dump.
///
/// The `##[group]Run …` line is useful reproduction context. The selected diagnostic and its following
/// source location take precedence over oversized wrapper arguments. The
/// `env:` / `with:` dump that follows the command is never the evidence.
/// The remaining block is a bounded window around the diagnostic, capped at
/// `max_bytes` on that region rather than the log head.
fn render_failed_step_excerpt(log: &str, max_bytes: usize) -> FailedStepExcerpt {
    let lines = classify_log_lines(log);
    let command = lines
        .iter()
        .find(|(kind, _)| *kind == LineKind::RunCommand)
        .map(|(_, line)| *line);

    let anchor = diagnostic_anchor(&lines).or_else(|| {
        lines
            .iter()
            .position(|(kind, _)| *kind == LineKind::GenericTrailer)
    });

    let Some(anchor_idx) = anchor else {
        return FailedStepExcerpt {
            body: command.unwrap_or("").to_string(),
            has_anchor: false,
        };
    };

    const LINES_BEFORE: usize = 24;
    const LINES_AFTER: usize = 12;
    let kept: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| !kind.skip_from_excerpt())
        .map(|(idx, _)| idx)
        .collect();
    let anchor_in_kept = kept.iter().position(|idx| *idx == anchor_idx).unwrap_or(0);
    let start = anchor_in_kept.saturating_sub(LINES_BEFORE);
    let end = (anchor_in_kept + 1 + LINES_AFTER).min(kept.len());
    let mut window: Vec<&str> = kept[start..end].iter().map(|idx| lines[*idx].1).collect();
    if let Some(command) = command
        && !window.contains(&command)
    {
        window.insert(0, command);
    }
    let joined = window.join("\n");
    FailedStepExcerpt {
        body: cap_bytes_around_line(&joined, lines[anchor_idx].1, max_bytes),
        has_anchor: true,
    }
}

fn classify_log_lines(log: &str) -> Vec<(LineKind, &str)> {
    let mut in_param_block = false;
    let mut after_failures_header = false;
    let mut out = Vec::new();
    for line in log.lines() {
        let payload = signature_payload(line);
        let lowered = payload.trim();
        let indented = payload.starts_with(' ') || payload.starts_with('\t');
        let kind = if is_run_command_payload(lowered) {
            in_param_block = false;
            LineKind::RunCommand
        } else if lowered.contains("##[endgroup]") {
            in_param_block = false;
            LineKind::EndGroup
        } else if lowered == "env:" || lowered == "with:" {
            in_param_block = true;
            LineKind::ParamDump
        } else if in_param_block && (indented || lowered.is_empty()) {
            LineKind::ParamDump
        } else {
            in_param_block = false;
            if is_generic_trailer(lowered) {
                LineKind::GenericTrailer
            } else if is_compiler_diagnostic(lowered) {
                LineKind::CompilerDiagnostic
            } else if is_cargo_status(lowered) {
                LineKind::CargoStatus
            } else if lowered.contains("##[error]") {
                LineKind::ErrorAnnotated
            } else if is_runner_bookkeeping(lowered) || lowered.contains("##[group]") {
                LineKind::Bookkeeping
            } else if is_concrete_diagnostic(&payload, lowered, after_failures_header) {
                LineKind::ConcreteDiagnostic
            } else if is_error_marker_line(lowered) && !is_assertion_payload(lowered) {
                LineKind::Marker
            } else {
                LineKind::Content
            }
        };
        if lowered == "failures:" || lowered == "errors:" {
            after_failures_header = true;
        } else if is_libtest_stdout_header(lowered) || lowered.starts_with("test result:") {
            after_failures_header = false;
        }
        out.push((kind, line));
    }
    out
}

fn is_generic_trailer(lowered: &str) -> bool {
    is_generic_runner_completion(lowered)
        || is_generic_process_failed(lowered)
        || is_generic_action_failed(lowered)
        || is_cargo_test_wrapper(lowered)
        || is_nextest_cancellation(lowered)
        || is_nextest_summary(lowered)
        || lowered.contains("tests were not run due to test failure")
}

fn is_generic_runner_completion(lowered: &str) -> bool {
    let Some(message) = lowered.trim().strip_prefix("##[error]") else {
        return false;
    };
    let Some(exit_code) = message
        .trim()
        .strip_prefix("process completed with exit code ")
    else {
        return false;
    };
    let exit_code = exit_code.trim_end_matches('.');
    !exit_code.is_empty() && exit_code.chars().all(|ch| ch.is_ascii_digit())
}

fn is_generic_process_failed(lowered: &str) -> bool {
    let Some(message) = lowered.trim().strip_prefix("##[error]") else {
        return false;
    };
    let message = message.trim().trim_end_matches('.');
    let Some(rest) = message.strip_prefix("the process ") else {
        return false;
    };
    rest.contains(" failed with exit code ")
}

fn is_generic_action_failed(lowered: &str) -> bool {
    let Some(message) = lowered.trim().strip_prefix("##[error]") else {
        return false;
    };
    let message = message
        .trim()
        .trim_start_matches(|ch: char| !ch.is_ascii_alphabetic());
    message == "action failed"
}

fn is_cargo_test_wrapper(lowered: &str) -> bool {
    let message = lowered
        .trim()
        .strip_prefix("error:")
        .map(str::trim)
        .unwrap_or_else(|| lowered.trim());
    message.starts_with("test failed, to rerun pass")
        || message == "test run failed"
        || message.starts_with("process didn't exit successfully:")
}

fn is_nextest_cancellation(lowered: &str) -> bool {
    lowered
        .trim()
        .trim_end_matches(':')
        .trim()
        .starts_with("cancelling due to test failure")
}

fn is_nextest_summary(lowered: &str) -> bool {
    let trimmed = lowered.trim();
    trimmed.starts_with("summary [") && trimmed.contains("tests run:")
}

fn is_concrete_diagnostic(payload: &str, lowered: &str, after_failures_header: bool) -> bool {
    is_panic_line(lowered)
        || is_failed_test_result(lowered)
        || is_nextest_fail_line(lowered)
        || is_libtest_listed_failure_name(payload, after_failures_header)
}

fn is_panic_line(lowered: &str) -> bool {
    lowered.contains("panicked at")
        && (lowered.contains("thread '") || lowered.contains("thread \""))
}

fn is_failed_test_result(lowered: &str) -> bool {
    let Some(rest) = lowered.strip_prefix("test ") else {
        return false;
    };
    let Some((_, status)) = rest.rsplit_once(" ... ") else {
        return false;
    };
    let status = status.trim();
    status == "failed" || status.starts_with("failed ")
}

fn is_nextest_fail_line(lowered: &str) -> bool {
    let Some(rest) = lowered.trim().strip_prefix("fail") else {
        return false;
    };
    rest.trim_start().starts_with('[')
}

fn is_libtest_stdout_header(lowered: &str) -> bool {
    let trimmed = lowered.trim();
    trimmed.starts_with("---- ")
        && (trimmed.ends_with(" stdout ----") || trimmed.ends_with(" stderr ----"))
}

fn is_libtest_listed_failure_name(payload: &str, after_failures_header: bool) -> bool {
    if !after_failures_header {
        return false;
    }
    let indented = payload.starts_with(' ') || payload.starts_with('\t');
    if !indented {
        return false;
    }
    let trimmed = payload.trim();
    !trimmed.is_empty()
        && !trimmed.contains(' ')
        && !trimmed.starts_with("----")
        && !trimmed.starts_with("thread")
        && !trimmed.starts_with("error")
        && !trimmed.starts_with("note:")
        && !trimmed.starts_with("assertion")
}

fn is_assertion_payload(lowered: &str) -> bool {
    let trimmed = lowered.trim_start();
    trimmed.starts_with("left:")
        || trimmed.starts_with("right:")
        || trimmed.starts_with("left =")
        || trimmed.starts_with("right =")
}

fn is_diagnostic_content(line: &str) -> bool {
    let payload = signature_payload(line);
    let trimmed = payload.trim();
    !trimmed.is_empty() && !trimmed.starts_with("##[")
}

fn is_run_command_payload(payload: &str) -> bool {
    let lowered = payload.trim().to_ascii_lowercase();
    lowered.starts_with("##[group]run ") || lowered == "##[group]run"
}

fn is_runner_bookkeeping(lowered: &str) -> bool {
    lowered.starts_with("head is now at") || lowered.starts_with("syncing repository")
}

/// Unanchored marker hit that is an actual diagnostic, not a passing test or
/// cargo/libtest section header. Those headers are identical across distinct
/// panics, and success lines often contain `error`/`failure` in the test name.
fn is_error_marker_line(lowered: &str) -> bool {
    if is_libtest_non_diagnostic(lowered) {
        return false;
    }
    ERROR_MARKERS.iter().any(|marker| lowered.contains(marker))
}

fn is_libtest_non_diagnostic(lowered: &str) -> bool {
    let trimmed = lowered.trim();
    matches!(trimmed, "failures:" | "errors:" | "successes:")
        || trimmed.starts_with("test result:")
        || is_successful_test_result(trimmed)
}

/// `test <name> ... ok` / `ignored`, with an optional timing suffix.
fn is_successful_test_result(lowered: &str) -> bool {
    let Some(rest) = lowered.strip_prefix("test ") else {
        return false;
    };
    let Some((_, status)) = rest.rsplit_once(" ... ") else {
        return false;
    };
    let status = status.trim();
    status == "ok"
        || status.starts_with("ok ")
        || status == "ignored"
        || status.starts_with("ignored ")
}

/// Cap `text` at `max_bytes` while keeping `anchor_line`, not the head.
fn cap_bytes_around_line(text: &str, anchor_line: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let Some(anchor_start) = text.find(anchor_line) else {
        return truncate_bytes(text, max_bytes);
    };
    let anchor_len = anchor_line.len();
    if anchor_len >= max_bytes {
        return truncate_bytes(anchor_line, max_bytes);
    }
    let extra = max_bytes - anchor_len;
    let want_before = extra / 3;
    let mut start = anchor_start.saturating_sub(want_before);
    while start < anchor_start && !text.is_char_boundary(start) {
        start += 1;
    }
    if start > 0
        && let Some(newline) = text[start..anchor_start].find('\n')
    {
        start += newline + 1;
    }
    let mut end = start.saturating_add(max_bytes).min(text.len());
    if end < anchor_start + anchor_len {
        end = (anchor_start + anchor_len).min(text.len());
        start = end.saturating_sub(max_bytes);
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
    }
    while end > anchor_start + anchor_len && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len()
        && let Some(newline) = text[anchor_start + anchor_len..end].rfind('\n')
    {
        end = anchor_start + anchor_len + newline;
    }
    let mut out = String::new();
    if start > 0 {
        out.push_str("[...]\n");
    }
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push_str(&format!(
            "\n[... truncated at {max_bytes} B for the task description; the full excerpt is in \
             the sweep run's step output ...]"
        ));
    }
    out
}

/// `query_errors` entries for this cluster's failed-step log fetch, if any.
fn relevant_log_query_errors<'a>(evidence: &'a Value, runs: &[Value]) -> Vec<&'a Value> {
    let run_ids: BTreeSet<String> = runs
        .iter()
        .map(|run| value_string(run, "run_id"))
        .filter(|id| !id.is_empty())
        .collect();
    evidence
        .get("query_errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter(|error| {
                    let query = value_string(error, "query");
                    let run_id = value_string(error, "run_id");
                    matches!(query.as_str(), "run_logs" | "run_logs_all")
                        && run_ids.contains(&run_id)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Strip the `job<TAB>step<TAB>timestamp ` columns a runner log carries.
fn log_payload(line: &str) -> &str {
    let mut columns = line.splitn(3, '\t');
    let rest = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(_step), Some(rest)) => rest,
        _ => line,
    };
    match rest.split_once(' ') {
        Some((first, tail)) if first.contains('T') && first.ends_with('Z') => tail,
        _ => rest,
    }
}

/// Payload used for classification and the normalized signature: runner
/// columns removed, ANSI styling stripped, lowercased. The filed excerpt keeps
/// the raw line so evidence is not discarded.
fn signature_payload(line: &str) -> String {
    strip_ansi_sequences(log_payload(line)).to_ascii_lowercase()
}

/// Collapse the parts of a log line that vary between identical failures:
/// bare numbers, long hex blobs, and measurements whose unit is the only
/// stable part (nextest's `FAIL [ 1.399s]` is a different duration on every
/// rerun of the same failing test).
fn normalize_signature(lowered: &str) -> String {
    let mut out = String::with_capacity(lowered.len());
    let mut chars = lowered.chars().peekable();
    let mut last_was_space = false;
    while let Some(ch) = chars.next() {
        if ch.is_ascii_alphanumeric() {
            let mut token = String::from(ch);
            while chars.peek().is_some_and(char::is_ascii_alphanumeric) {
                token.push(chars.next().unwrap_or_default());
            }
            // The token is ASCII alphanumeric, so a digit count indexes it directly.
            let digits = token.chars().take_while(char::is_ascii_digit).count();
            if digits == token.len() {
                out.push_str("<n>");
            } else if token.len() >= 7 && token.chars().all(|c| c.is_ascii_hexdigit()) {
                out.push_str("<hex>");
            } else if digits > 0 && token[digits..].chars().all(|c| c.is_ascii_alphabetic()) {
                // A measurement such as `399s` or `250ms`: keep the unit, drop the count.
                out.push_str("<n>");
                out.push_str(&token[digits..]);
            } else {
                out.push_str(&token);
            }
            last_was_space = false;
            continue;
        }
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        out.push(ch);
        last_was_space = false;
    }
    truncate_chars(out.trim(), 200)
}

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
        .chars()
        .take(KEY_LEN)
        .collect()
}

fn bounded_u64(input: &Value, key: &str, default: u64, max: u64) -> Result<u64, OrbitError> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let raw = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| OrbitError::InvalidInput(format!("input.{key} must be a positive integer")))?;
    if raw == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "input.{key} must be greater than zero"
        )));
    }
    Ok(raw.min(max))
}

/// Read a snapshot field as a display string, accepting the numeric spellings
/// `gh` uses for run and job identifiers.
fn value_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn run_order(run: &Value) -> (String, u64) {
    (
        value_string(run, "created_at"),
        run.get("run_id").and_then(Value::as_u64).unwrap_or(0),
    )
}

fn display(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect::<String>() + "…"
}

fn truncate_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[... truncated at {max_bytes} B for the task description; the full excerpt is in \
         the sweep run's step output ...]",
        &value[..end]
    )
}
