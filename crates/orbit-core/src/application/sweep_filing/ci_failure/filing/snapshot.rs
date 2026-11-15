//! Snapshot preparation: read the `ci_evidence` snapshot, settle every ending
//! that files nothing (unreadable schema, rate limit, no collection,
//! snapshot-wide gaps), and split the current failures into the complete set
//! to file and the deferred, infrastructure, already-repaired and
//! task-branch sets reported beside it.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::sweep_filing::bounded_u64;
use crate::application::sweep_filing::duplicate_tasks::DuplicateTaskLookup;
use crate::application::task::TaskAddParams;

use super::super::cancellation::{
    drop_inconclusive_log_errors, inconclusive_audit, split_inconclusive_cancellations,
};
use super::super::cluster::FailureCluster;
use super::super::evidence::{
    audit_summary, deferral_audit, exclude_already_repaired, normalize_retryable_error,
    partition_retryable_errors, repaired_audit, retryable_pipeline_error, run_id_key,
    single_retryable_error, split_deferred_failures,
};
use super::super::grouping::cluster_failures;
use super::super::infrastructure::{infrastructure_audit, split_declared_infrastructure};
use super::branch::{BranchPartition, partition_branch_failures};
use super::{
    DEFAULT_MAX_TASKS, MAX_MAX_TASKS, OUTCOME_CAPABILITY_UNAVAILABLE, OUTCOME_DEFERRED,
    OUTCOME_HINT_RATE_LIMITED, SUPPORTED_SCHEMA_VERSION,
};

/// A prepared snapshot, or the outcome that ends the filing before any
/// cluster is formed.
pub(super) enum SnapshotPreparation<'a> {
    Finished(Value),
    Ready(Box<FilingSnapshot<'a>>),
}

/// Everything the later phases read from one prepared snapshot.
pub(super) struct FilingSnapshot<'a> {
    pub(super) evidence: &'a Value,
    pub(super) capability: Value,
    pub(super) max_tasks: usize,
    pub(super) audit: Value,
    pub(super) complete: Vec<Value>,
    pub(super) deferred: Vec<Value>,
    pub(super) evidence_incomplete: Vec<Value>,
    pub(super) inconclusive: Vec<Value>,
    pub(super) infrastructure: Vec<Value>,
    pub(super) already_repaired: Vec<Value>,
    pub(super) pending_supersession: Vec<Value>,
    pub(super) branch_observations: Vec<(String, Value)>,
    pub(super) excluded_branch_failures: Vec<Value>,
}

/// Snapshot preparation for `file_ci_failure_tasks_with_ops`. A landing
/// failure whose evidence stayed incomplete is filed here, through
/// `add_task`, before anything else is decided.
pub(super) fn prepare_snapshot<'a, L, F>(
    runtime: &OrbitRuntime,
    input: &'a Value,
    lookup: &L,
    add_task: &mut F,
) -> Result<SnapshotPreparation<'a>, OrbitError>
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

    // GitHub rate-limited the collection's reads: the sweep waits for the
    // reset rather than failing over gaps that are about the quota.
    if evidence.get("outcome_hint").and_then(Value::as_str) == Some(OUTCOME_HINT_RATE_LIMITED) {
        let audit = audit_summary(evidence, &[]);
        let rate_limit = evidence.get("rate_limit").cloned().unwrap_or(Value::Null);
        let not_before = rate_limit
            .get("not_before")
            .and_then(Value::as_str)
            .unwrap_or("the reset");
        return Ok(SnapshotPreparation::Finished(json!({
            "outcome": OUTCOME_DEFERRED,
            "capability": capability,
            "rate_limit": rate_limit,
            "clusters": 0,
            "filed_count": 0,
            "filed": [],
            "pilot_candidate_count": 0,
            "pilot_candidates": [],
            "skipped_existing": [],
            "withheld": [],
            "skipped_over_cap": [],
            "deferred_count": 0,
            "deferred": [],
            "audit": audit,
            "detail": format!(
                "GitHub rate-limited this host's reads, so the sweep is deferred until \
                 {not_before}; no task was filed and this is not a CI pass"
            ),
        })));
    }
    // The collection stage could not look. Every list below would be empty and
    // would read exactly like "nothing is failing" — a conclusion that needs
    // queries this host never ran.
    if evidence.get("collected").and_then(Value::as_bool) != Some(true) {
        let audit = audit_summary(evidence, &[]);
        return Ok(SnapshotPreparation::Finished(json!({
            "outcome": OUTCOME_CAPABILITY_UNAVAILABLE,
            "capability": capability,
            "clusters": 0,
            "filed_count": 0,
            "filed": [],
            "pilot_candidate_count": 0,
            "pilot_candidates": [],
            "skipped_existing": [],
            "withheld": [],
            "skipped_over_cap": [],
            "deferred": [],
            "audit": audit,
            "detail": "no CI evidence was gathered, so no task was filed; this is not a CI pass",
        })));
    }

    let max_tasks = bounded_u64(input, "max_tasks", DEFAULT_MAX_TASKS, MAX_MAX_TASKS)? as usize;
    let findings = ["current_failures", "branch_failures"]
        .into_iter()
        .filter_map(|key| evidence.get(key).and_then(Value::as_array))
        .flatten()
        .cloned()
        .collect();
    let (failures, inconclusive) = split_inconclusive_cancellations(
        findings,
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
        // The gaps are retryable, so hold the sweep for a later pass instead
        // of failing the run: return the deferred outcome with the audit
        // payload the retryable error used to carry, and file nothing.
        let mut held_audit = audit.clone();
        held_audit["retryable_errors"] = json!(retryable_errors.len());
        return Ok(SnapshotPreparation::Finished(json!({
            "outcome": OUTCOME_DEFERRED,
            "stage": "collection_or_investigation",
            "capability": capability,
            "clusters": 0,
            "filed_count": 0,
            "filed": [],
            "pilot_candidate_count": 0,
            "pilot_candidates": [],
            "skipped_existing": [],
            "withheld": [],
            "skipped_over_cap": [],
            "deferred_count": 0,
            "deferred": [],
            "retryable_errors": retryable_errors,
            "audit": held_audit,
            "detail": "collection was incomplete because of retryable errors, so the \
                       sweep is deferred to a later sweep; no task was filed from this \
                       incomplete snapshot and this is not a CI pass",
        })));
    }
    // A landing failure collection could not evidence across its retry window
    // gets an owner now, whatever else this snapshot still defers.
    let evidence_incomplete = super::super::escalation::file_landing_evidence_incomplete(
        evidence, lookup, &audit, add_task,
    )?;
    let mut audit = audit;
    audit["evidence_incomplete_task_ids"] = json!(
        evidence_incomplete
            .iter()
            .filter_map(|entry| entry.get("task_id").cloned())
            .collect::<Vec<_>>()
    );
    let (complete, mut deferred) = split_deferred_failures(&failures, &run_errors, schema_version);
    let (complete, already_repaired) = exclude_already_repaired(complete, evidence);
    let audit = repaired_audit(audit, &already_repaired);
    let (complete, infrastructure) = split_declared_infrastructure(
        complete,
        &runtime
            .context
            .settings()
            .pr_settings()
            .ci_infrastructure_signatures,
    );
    let audit = infrastructure_audit(audit, &infrastructure);
    // Recheck old/replayed snapshots too: ref_kind and the PR/event head alone
    // do not prove the failing checkout belongs to a landing branch.
    let BranchPartition {
        complete,
        branch_observations,
        excluded_branch_failures,
    } = partition_branch_failures(runtime, evidence, complete, &mut deferred);
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
    // Failures collection held back while a descendant's run is in flight.
    // Filing adds the ones whose repair already landed on a descendant.
    let pending_supersession = evidence
        .get("pending_supersession")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(SnapshotPreparation::Ready(Box::new(FilingSnapshot {
        evidence,
        capability,
        max_tasks,
        audit,
        complete,
        deferred,
        evidence_incomplete,
        inconclusive,
        infrastructure,
        already_repaired,
        pending_supersession,
        branch_observations,
        excluded_branch_failures,
    })))
}

/// Cluster the complete failures by root cause. Failures that exist but form
/// no cluster are a retryable registration error, never an empty sweep.
pub(super) fn register_clusters(
    snapshot: &FilingSnapshot<'_>,
) -> Result<Vec<FailureCluster>, OrbitError> {
    let clusters = cluster_failures(&snapshot.complete);

    if !snapshot.complete.is_empty() && clusters.is_empty() {
        return Err(retryable_pipeline_error(
            "registration",
            &snapshot.audit,
            vec![single_retryable_error(
                "registration",
                "cluster_failures",
                json!({}),
                "current failures were discovered but none could be registered for task filing",
            )],
        ));
    }
    Ok(clusters)
}

pub(super) fn empty_snapshot_detail(
    inconclusive_empty: bool,
    deferred_empty: bool,
) -> &'static str {
    if !inconclusive_empty {
        "the queries ran and found no current, non-superseded failure; cancelled jobs without failed steps remain explicit inconclusive evidence, not a pass"
    } else if !deferred_empty {
        "task-branch evidence on a claimed owner is deferred until that claim settles; landing failures in this snapshot are filed separately"
    } else {
        "no landing-branch repair remains; task-branch evidence is retained on its owner and other branch failures are excluded"
    }
}
