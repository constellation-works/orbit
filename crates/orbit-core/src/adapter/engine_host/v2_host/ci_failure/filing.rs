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

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::admission::duplicate_tasks::{
    DuplicateTaskLookup, DuplicateTaskMatch, SnapshotDuplicateLookup,
};
use crate::adapter::engine_host::v2_host::admission::sweep_filing::bounded_u64;
use crate::application::task::TaskAddParams;

use super::cancellation::{
    drop_inconclusive_log_errors, inconclusive_audit, split_inconclusive_cancellations,
};
use super::evidence::{
    audit_summary, bounded_error, deferral_audit, deferred_errors, exclude_already_repaired,
    filing_audit, normalize_retryable_error, partition_retryable_errors, repaired_audit,
    retryable_pipeline_error, run_id_key, split_deferred_failures,
};
use super::grouping::cluster_failures;
use super::repair_assessment;

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
pub(super) const CI_FAILURE_SWEEP_TITLE_PREFIX: &str = "[ci-failure-sweep] ";
/// The system crew name. Filed tasks belong on the system lane, matching the
/// shipped `ci-failure-remediation` auto-task — but this is a plain default,
/// not a hard-coded assumption that the lane is configured: a workspace whose
/// crew roster has no `system` entry still gets its task filed, just without
/// a crew set.
const SYSTEM_CREW: &str = "system";

const DEFAULT_MAX_TASKS: u64 = 5;
const MAX_MAX_TASKS: u64 = 20;
/// Log bytes carried into a task description. `collect_ci_evidence` has already
/// bounded and redacted the excerpt; this is a second, tighter bound so a
/// description stays a readable brief.
pub(super) const DESCRIPTION_LOG_BYTES: usize = 4_000;
/// Runs listed per cluster in the description.
pub(super) const MAX_LISTED_RUNS: usize = 6;

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
    // Every cluster and legacy key is assessed against one snapshot: the
    // task list is hydrated at most once per filing and each open task's
    // comments are read at most once, however many clusters the run has.
    let lookup = SnapshotDuplicateLookup::new(lookup);
    let mut assessor = repair_assessment::Assessor::new(runtime);
    let mut repair_assessments = Vec::new();
    let duplicate_matches = clusters
        .iter()
        .map(|cluster| {
            let existing = cluster.find_covering_task(&lookup).map_err(|error| {
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
